// admissions module — Helpers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
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

use super::constants::{MAX_ID, MAX_MONEY, MAX_SHORT, PERM_ADMISSIONS_READ_ALL};
use super::state::AdmissionsState;
use super::types::Application;

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, validation, views
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// Money rounded to four decimals (NUMERIC(19,4)).
pub(crate) fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
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

/// Ask the roles module whether `role` holds `permission` through the
/// `role.check` hook. Fails closed: no answer means not allowed.
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
// Tenant edge guard (C3 pilot 4, gate-only).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` / `authorize` or any
/// capability check.
///
/// This module's records carry no `company_id` / `tenant_id` scope
/// field, so the guard is a fail-closed gate only: no per-record
/// company check is applied. Real partitioning of these records
/// needs a `tenant_id` on the record (C11 follow-up).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// Company scope of the verified caller: the tenant context's
/// company first, the server-resolved caller company second; a
/// mismatch between the two is a 403. The `X-Company-Id` header is
/// never read. Resolved in every gated handler so a tenant/caller
/// mismatch fails closed even though these records carry no company
/// field to filter on (gate-only; see `require_tenant`).
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
        (None, verified) => Ok(verified.clone()),
    }
}

/// True when the caller may read `application`: staff with read-all,
/// or the applicant themself.
pub(crate) fn can_read_application(ctx: &AppContext, caller: &Caller, application: &Application) -> bool {
    check_capability(ctx, &caller.role, PERM_ADMISSIONS_READ_ALL)
        || application.applicant_user_id == caller.actor
}

/// Enforce read access for one application, hiding the not-found
/// case: a caller who may not read the record gets 404, so
/// application ids cannot be probed.
pub(crate) fn load_application_for_read(
    state: &AdmissionsState,
    ctx: &AppContext,
    caller: &Caller,
    application_id: &str,
) -> UnifiedResult<Application> {
    let application = state.application(application_id)?;
    if can_read_application(ctx, caller, &application) {
        Ok(application)
    } else {
        Err(UnifiedError::not_found("Application not found"))
    }
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
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

pub(crate) fn require_id(value: &str, field: &str) -> UnifiedResult<String> {
    require_text(value, field, MAX_ID)
}

pub(crate) fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

pub(crate) fn parse_money(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value >= MAX_MONEY {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite amount from 0 up to 999999999999999.9999"
        )));
    }
    Ok(round4(value))
}

pub(crate) fn valid_currency(raw: &str) -> UnifiedResult<String> {
    let currency = raw.trim().to_uppercase();
    if currency.len() != 3 || !currency.chars().all(|ch| ch.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code (for example USD)",
        ));
    }
    Ok(currency)
}

/// Exactly one `@`, non-empty local and domain parts, and a dot in
/// the domain. Rejects the shapes other modules reject too
/// (`a@b`, `a@b@c.com`, `a@.b.com`, `a@b.com.`).
pub(crate) fn valid_email(raw: &str) -> UnifiedResult<String> {
    let email = raw.trim().to_lowercase();
    if email.len() > MAX_SHORT || email.is_empty() || email.contains(' ') {
        return Err(UnifiedError::bad_request(
            "email must be a valid email address",
        ));
    }
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    let valid = !local.is_empty()
        && !domain.is_empty()
        && parts.next().is_none()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.');
    if !valid {
        return Err(UnifiedError::bad_request(
            "email must be a valid email address",
        ));
    }
    Ok(email)
}

/// `Birth Certificate` -> `birth-certificate`.
pub(crate) fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
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

#[cfg(test)]
mod tests {
    use super::*;

    fn params_with_search(search: &str) -> PaginationParams {
        PaginationParams {
            search: Some(search.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn round4_rounds_to_four_decimals() {
        assert!((round4(1.23456) - 1.2346).abs() < 1e-9);
        assert!((round4(2.5) - 2.5).abs() < 1e-9);
        assert!((round4(10.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn slugify_lowercases_and_dashes_words() {
        assert_eq!(slugify("Birth Certificate"), "birth-certificate");
        assert_eq!(slugify("  A  B  "), "a-b");
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn valid_currency_normalises_to_uppercase_iso_codes() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency(" eur ").unwrap(), "EUR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("u1d").is_err());
    }

    #[test]
    fn valid_email_normalises_and_rejects_bad_addresses() {
        assert_eq!(
            valid_email("User@Example.com").unwrap(),
            "user@example.com"
        );
        assert_eq!(
            valid_email("first.last+tag@example.co.id").unwrap(),
            "first.last+tag@example.co.id"
        );
        assert!(valid_email("no-at-sign").is_err());
        assert!(valid_email("@example.com").is_err());
        assert!(valid_email("user@").is_err());
        assert!(valid_email("user name@example.com").is_err());
        assert!(valid_email("a@b@c.com").is_err());
        assert!(valid_email("a@b").is_err());
        assert!(valid_email("a@.b.com").is_err());
        assert!(valid_email("a@b.com.").is_err());
        assert!(valid_email("").is_err());
    }

    #[test]
    fn parse_money_rounds_and_rejects_out_of_range_amounts() {
        assert!((parse_money(10.123456, "fee").unwrap() - 10.1235).abs() < 1e-9);
        assert!((parse_money(0.0, "fee").unwrap() - 0.0).abs() < 1e-9);
        assert!(parse_money(-1.0, "fee").is_err());
        assert!(parse_money(f64::NAN, "fee").is_err());
        assert!(parse_money(1_000_000_000_000_000.0, "fee").is_err());
    }

    #[test]
    fn parse_date_accepts_iso_dates_only() {
        assert_eq!(
            parse_date("2026-01-15", "start").unwrap().to_string(),
            "2026-01-15"
        );
        assert!(parse_date("15/01/2026", "start").is_err());
        assert!(parse_date("2026-13-01", "start").is_err());
    }

    #[test]
    fn require_text_trims_and_enforces_length() {
        assert_eq!(require_text("  ok  ", "name", 10).unwrap(), "ok");
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("toolong", "name", 3).is_err());
        assert_eq!(optional_text(&None, "name", 10).unwrap(), None);
        assert!(require_id(&"x".repeat(MAX_ID + 1), "id").is_err());
    }

    #[test]
    fn matches_search_is_case_insensitive_and_empty_means_match() {
        assert!(matches_search("Foo Bar", &params_with_search("foo")));
        assert!(!matches_search("Foo Bar", &params_with_search("zzz")));
        assert!(matches_search("Foo Bar", &PaginationParams::default()));
    }

    #[test]
    fn paginate_slices_pages_and_reports_totals() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
        let last = PaginationParams {
            page: Some(99),
            per_page: Some(2),
            ..Default::default()
        };
        assert!(paginate(vec![1, 2, 3, 4, 5], &last).items.is_empty());
    }
}
