// appointments - common.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::scheduling::*;
use super::reminders::*;
use super::ics::*;
use super::validation::*;
use super::handlers_services::*;
use super::handlers_staff::*;
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

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

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Records here carry `company_id: Option<String>` (empty/missing
/// means unscoped), so the scope helpers below compare strings.
/// The public booking surface (`guest_book`,
/// `public_booking_lookup`, `list_public_services`,
/// `list_public_service_slots`) is deliberately NOT gated: it is
/// anonymous by design and carries no `HttpRequest` tenant
/// context to resolve.
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
/// middleware-resolved tenant context are sources.
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


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
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
/// `CallerScope::Unresolved` rule). With a scope, only records carrying
/// exactly that company match, plus unscoped (global/legacy) records,
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > max {
                Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        None => Ok(None),
    }
}

pub(crate) fn valid_email(email: &str) -> UnifiedResult<String> {
    let trimmed = email.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_EMAIL_LEN || trimmed.contains(' ') {
        return Err(UnifiedError::bad_request("Invalid email address"));
    }
    let mut parts = trimmed.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return Err(UnifiedError::bad_request("Invalid email address"));
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return Err(UnifiedError::bad_request("Invalid email address"));
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err(UnifiedError::bad_request("Invalid email address"));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn valid_user_id(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_USER_ID_LEN {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty and must be at most {MAX_USER_ID_LEN} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// Money: a price in minor units, non-negative and at most
/// `MAX_PRICE`. The request JSON already converted major units to
/// minor units through `serde_major` (half away from zero to minor
/// units), so this only validates the range.
pub(crate) fn valid_price(price: i64) -> UnifiedResult<i64> {
    if price < 0 || price > MAX_PRICE {
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

pub(crate) fn valid_channels(channels: &[String]) -> UnifiedResult<Vec<String>> {
    if channels.is_empty() {
        return Err(UnifiedError::bad_request("At least one channel is required"));
    }
    let mut out = Vec::with_capacity(channels.len());
    for channel in channels {
        let trimmed = channel.trim();
        if trimmed.len() > MAX_CHANNEL_LEN || !NOTIFICATION_CHANNELS.contains(&trimmed) {
            return Err(UnifiedError::bad_request(format!(
                "Channels must be a subset of: {}",
                NOTIFICATION_CHANNELS.join(", ")
            )));
        }
        if !out.contains(&trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

pub(crate) fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date in YYYY-MM-DD format")))
}

pub(crate) fn parse_datetime(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    raw.trim()
        .parse::<DateTime<Utc>>()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 date-time")))
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

pub(crate) fn confirmation_code() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| {
            let index = rng.random_range(0..CONFIRMATION_CODE_CHARS.len());
            CONFIRMATION_CODE_CHARS[index] as char
        })
        .collect()
}

/// Client key for the public rate limit: the first address in
/// `X-Forwarded-For`, else `X-Real-IP`, else "unknown". The key is
/// never stored beyond the last hour's attempt timestamps.
pub(crate) fn client_key(req: &HttpRequest) -> String {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            req.headers()
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("unknown")
        .to_string()
}

/// Record one public booking attempt for `key` and fail with 429
/// when the hourly limit from the settings is reached. Attempts
/// older than one hour are pruned on every call.
pub(crate) fn check_guest_rate_limit(state: &AppointmentsState, key: &str, limit: i64) -> UnifiedResult<()> {
    let now = Utc::now();
    let cutoff = now - Duration::hours(1);
    let mut entry = state.guest_attempts.entry(key.to_string()).or_default();
    entry.retain(|instant| *instant > cutoff);
    if entry.len() as i64 >= limit.max(1) {
        return Err(UnifiedError::new(
            "Too many booking attempts. Please try again later.",
            429,
        ));
    }
    entry.push(now);
    Ok(())
}

// Loaders

pub(crate) fn load_service(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<AppointmentService> {
    let id = parse_uuid(raw_id, "service")?;
    state
        .services
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Service not found: {id}")))
}

pub(crate) fn load_service_by_id(state: &AppointmentsState, id: Uuid) -> UnifiedResult<AppointmentService> {
    state
        .services
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Service not found: {id}")))
}

pub(crate) fn load_appointment(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<Appointment> {
    let id = parse_uuid(raw_id, "appointment")?;
    state
        .appointments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Appointment not found: {id}")))
}

pub(crate) fn load_availability(
    state: &AppointmentsState,
    staff_user_id: &str,
    raw_id: &str,
) -> UnifiedResult<StaffAvailability> {
    let id = parse_uuid(raw_id, "availability")?;
    state
        .availability
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|row| row.staff_user_id == staff_user_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Availability not found: {id}")))
}

pub(crate) fn load_reminder(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<AppointmentReminder> {
    let id = parse_uuid(raw_id, "reminder")?;
    state
        .reminders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reminder not found: {id}")))
}

pub(crate) fn load_rule(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<ReminderRule> {
    let id = parse_uuid(raw_id, "reminder rule")?;
    state
        .reminder_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reminder rule not found: {id}")))
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The tenant context the tenant-resolution middleware would
    /// attach for a request on a tenant host
    /// (`unified_core::middleware::TenantContext`, the exact struct
    /// `tenancy::TenantContext::from_http_request` reads from the
    /// request extensions). The company is empty, so the resolved
    /// scope is unscoped.
    fn tenant_request() -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            },
        );
        req
    }

        /// A request with a tenant context carrying `tenant_company`
    /// (empty = no tenant company), a verified caller carrying
    /// `verified_company` (empty = no company resolved), and a
    /// forged `X-Company-Id` header when `forged_header` is
    /// non-empty. The header must never affect the scope.
    fn scoped_request(
        tenant_company: &str,
        verified_company: &str,
        forged_header: &str,
    ) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let mut req = actix_web::test::TestRequest::default();
        if !forged_header.is_empty() {
            req = req.insert_header(("x-company-id", forged_header));
        }
        let req = req.to_http_request();
        if !verified_company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(verified_company.to_string()),
            );
        }
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: tenant_company.to_string(),
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
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = scoped_request("company-a", "company-a", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_uses_verified_caller_when_tenant_has_none() {
        let req = scoped_request("", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-b".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = scoped_request("company-a", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert!(super::caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_a_forged_company_header() {
        let req = scoped_request("", "", "company-forged");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(super::caller_company(&tenant, &req).unwrap(), None);
    }

#[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
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
    fn email_validation_trims_and_rejects_bad_shapes() {
        assert_eq!(valid_email("  ada@example.com ").unwrap(), "ada@example.com");
        assert!(valid_email("ada@example").is_err());
        assert!(valid_email("ada@.example.com").is_err());
        assert!(valid_email("ada@example.com.").is_err());
        assert!(valid_email("ada@@example.com").is_err());
        assert!(valid_email("ada example@example.com").is_err());
        assert!(valid_email("").is_err());
    }

    #[test]
    fn currency_codes_are_normalized_to_uppercase() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency("EUR").unwrap(), "EUR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USD1").is_err());
        assert!(valid_currency("U D").is_err());
    }

    #[test]
    fn prices_accept_minor_units_and_reject_bad_values() {
        assert_eq!(valid_price(1999).unwrap(), 1999);
        assert_eq!(valid_price(0).unwrap(), 0);
        assert!(valid_price(-1).is_err());
        assert!(valid_price(MAX_PRICE + 1).is_err());
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
        assert_eq!(apply_percent_to_minor(10000, 50.0), 5000);
    }

    #[test]
    fn date_parsing_accepts_only_iso_dates() {
        assert!(parse_date("2026-11-22", "date").is_ok());
        assert!(parse_date("2026-02-30", "date").is_err());
        assert!(parse_date("22/11/2026", "date").is_err());
        assert!(parse_datetime("2026-11-22T12:00:00Z", "at").is_ok());
        assert!(parse_datetime("2026-11-22 12:00", "at").is_err());
    }

    #[test]
    fn confirmation_codes_use_the_unambiguous_alphabet() {
        let code = confirmation_code();
        assert_eq!(code.len(), 8);
        assert!(
            code.chars()
                .all(|c| "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c))
        );
    }
}
