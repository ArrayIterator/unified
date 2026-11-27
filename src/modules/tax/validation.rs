// Tax module - caller, capability, and input validation helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// Helpers

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn today() -> NaiveDate {
    Utc::now().date_naive()
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

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check
/// (C3 pilot 3).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// Parse one company source value as a UUID. Company ids in this
/// module are UUIDs; a source that carries a non-UUID value (or the
/// nil UUID, which is never a company) is treated as absent -- its
/// namespace is not this module's -- never as a company. The
/// fail-closed rule in `acting_company` still refuses company-less
/// callers outright.
fn parse_company_id(raw: &str) -> Option<Uuid> {
    Uuid::parse_str(raw.trim()).ok().filter(|id| !id.is_nil())
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both resolve to different
/// companies. The `X-Company-Id` header is NEVER trusted here: only
/// the verified caller (company resolved from the database at
/// authentication time) and the middleware-resolved tenant context
/// are sources. The `user.company_requested` hook remains a last
/// resort inside `acting_company` only (see there).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<Uuid>> {
    let tenant_company = tenant.company_id.as_deref().and_then(parse_company_id);
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .as_deref()
        .and_then(parse_company_id);
    match (tenant_company, verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company)),
        (None, verified) => Ok(verified),
    }
}

/// True when a stored record belongs to the caller's company scope.
/// Tax records always carry a company (`company_id: Uuid`, never
/// global), so an unresolved caller matches nothing -- there are no
/// global rows to fall back to (the shared
/// `CallerScope::Unresolved` rule, same as timetracker's
/// `None => false`).
pub(crate) fn in_company_scope(
    record_company_id: Uuid,
    caller_company: &Option<Uuid>,
) -> bool {
    match caller_company {
        None => false,
        Some(company) => *company == record_company_id,
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers filter on
/// `in_company_scope` (or an equality check against the acting
/// company) instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: Uuid,
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
    record_company_id: Uuid,
    caller_company: &Option<Uuid>,
) -> UnifiedResult<()> {
    match caller_company {
        Some(caller) if *caller == record_company_id => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// The company the caller acts for: the tenant context's company
/// first, then the verified caller's server-resolved company, then
/// the `user.company_requested` hook answered by the users module
/// (see `caller_company` and `resolve_caller_company`). The
/// `X-Company-Id` header is NEVER read here: it is client-asserted,
/// and trusting it let any caller holding the tax permission claim
/// any company id and read another tenant's data.
pub(crate) fn resolve_caller_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> Option<Uuid> {
    let _ = req;
    let hook = ctx
        .hooks
        .emit("user.company_requested", json!({ "user_id": caller.actor }));
    hook.get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// Company scoping for writes: a caller who belongs to a company may
/// only manage records of that company. A caller with no resolvable
/// company (unresolved) may manage nothing: platform access is never
/// the default for a missing company (the shared
/// `CallerScope::Unresolved` rule); an explicit platform decision is
/// required and this helper does not make one.
pub(crate) fn require_company_scope(
    caller_company: Option<Uuid>,
    target_company: Uuid,
) -> UnifiedResult<()> {
    match caller_company {
        Some(company) if company != target_company => Err(UnifiedError::forbidden(
            "A company admin may only manage records of their own company",
        )),
        Some(_) => Ok(()),
        None => Err(UnifiedError::forbidden(
            "A caller with no resolvable company may not manage records of any company",
        )),
    }
}

/// True when the company module confirms the company exists, through
/// the `company.exists` hook. Fail-closed when the hook is answered;
/// when no handler is registered the company module is absent and
/// the check is skipped.
pub(crate) fn company_exists(ctx: &AppContext, company_id: Uuid) -> bool {
    if !ctx.hooks.has("company.exists") {
        return true;
    }
    let answer = ctx.hooks.emit(
        "company.exists",
        json!({ "company_id": company_id.to_string() }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("exists"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn require_company(ctx: &AppContext, company_id: Uuid) -> UnifiedResult<()> {
    if company_exists(ctx, company_id) {
        Ok(())
    } else {
        Err(UnifiedError::not_found(format!(
            "Company {company_id} does not exist"
        )))
    }
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

/// Resolve the acting company for a request: the caller must
/// belong to a company resolvable through the tenant context, the
/// verified caller, or the `user.company_requested` hook, and the
/// company must exist. An unresolved caller is refused outright:
/// tax has no global rows, so there is nothing safe to fall back
/// to (the shared `CallerScope::Unresolved` rule).
pub(crate) fn acting_company(
    ctx: &AppContext,
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    caller: &Caller,
) -> UnifiedResult<Uuid> {
    let company_id = caller_company(tenant, req)?
        .or_else(|| resolve_caller_company(ctx, req, caller))
        .ok_or_else(|| {
            UnifiedError::bad_request(
                "Missing company: belong to a company resolvable through the tenant context, the verified caller, or user.company_requested",
            )
        })?;
    require_company(ctx, company_id)?;
    Ok(company_id)
}

pub(crate) fn parse_date(value: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

pub(crate) fn valid_currency(code: &str) -> bool {
    code.len() == 3 && code.chars().all(|ch| ch.is_ascii_uppercase())
}

pub(crate) fn valid_country_code(code: &str) -> bool {
    code.len() == 2 && code.chars().all(|ch| ch.is_ascii_alphabetic())
}

pub(crate) fn valid_region_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 16
        && code
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

pub(crate) fn escape_csv(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_currency_needs_three_uppercase_chars() {
        assert!(valid_currency("USD"));
        // Regression: digits used to pass ("US1"), and the test
        // itself asserted that. A currency code is three letters.
        assert!(!valid_currency("US1"));
        assert!(!valid_currency("U12"));
        assert!(!valid_currency("usd"));
        assert!(!valid_currency("US"));
        assert!(!valid_currency("USDD"));
        assert!(!valid_currency("U D"));
    }

    #[test]
    fn valid_country_code_needs_two_letters() {
        assert!(valid_country_code("ID"));
        assert!(valid_country_code("id"));
        assert!(!valid_country_code("I"));
        assert!(!valid_country_code("IDN"));
        assert!(!valid_country_code("1D"));
    }

    #[test]
    fn valid_region_code_rules() {
        assert!(valid_region_code("JK"));
        assert!(valid_region_code("west-java_1"));
        assert!(!valid_region_code(""));
        assert!(!valid_region_code(&"a".repeat(17)));
        assert!(!valid_region_code("has space"));
    }

    #[test]
    fn escape_csv_quotes_only_when_needed() {
        assert_eq!(escape_csv("plain"), "plain");
        assert_eq!(escape_csv("a,b"), "\"a,b\"");
        assert_eq!(escape_csv("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn parse_date_accepts_iso_dates_only() {
        assert_eq!(
            parse_date("2025-01-31", "from").unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 31).unwrap()
        );
        assert!(parse_date("31/01/2025", "from").is_err());
        assert!(parse_date("2025-13-01", "from").is_err());
        assert!(parse_date(" 2025-02-28 ", "from").is_ok());
    }

    #[test]
    fn require_company_scope_denies_unresolved_callers() {
        let company = Uuid::now_v7();
        assert!(require_company_scope(Some(company), company).is_ok());
        assert!(require_company_scope(Some(Uuid::now_v7()), company).is_err());
        assert!(require_company_scope(None, company).is_err());
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

    fn tenant_ctx(req: &HttpRequest) -> unified_core::tenancy::TenantContext {
        unified_core::tenancy::TenantContext::from_http_request(req)
            .expect("tenant context must resolve")
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let company = Uuid::now_v7();
        let req = tenant_request(&company.to_string(), Some(&company.to_string()));
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_falls_back_to_verified_caller_company() {
        let company = Uuid::now_v7();
        let req = tenant_request("", Some(&company.to_string()));
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_tenant_and_verified_company_mismatch() {
        let req = tenant_request(
            &Uuid::now_v7().to_string(),
            Some(&Uuid::now_v7().to_string()),
        );
        let tenant = tenant_ctx(&req);
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_never_trusts_the_company_header() {
        // A forged X-Company-Id header grants nothing: with no
        // tenant company and no verified caller company, the scope
        // stays unresolved even when the header names a company.
        let claimed = Uuid::now_v7();
        let req = TestRequest::default()
            .insert_header(("x-company-id", claimed.to_string()))
            .to_http_request();
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
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_matches_nothing_for_unresolved_callers() {
        // Tax records always carry a company: there are no global
        // rows, so an unresolved caller matches nothing at all.
        let company = Uuid::now_v7();
        assert!(!in_company_scope(company, &None));
        assert!(in_company_scope(company, &Some(company)));
        assert!(!in_company_scope(company, &Some(Uuid::now_v7())));
    }
}
