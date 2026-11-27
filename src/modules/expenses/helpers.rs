// Helpers: callers, validation, responses, transitions.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use crate::modules::entitlements::TenantScope;
use actix_web::{HttpRequest, HttpResponse};
use chrono::{NaiveDate, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `unified_core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// - when it lands - takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// Per-record tenant filtering is closed by `tenant_scope` /
/// `require_company_scope` below: every tenant row (report,
/// category, policy, and everything reached through a report)
/// carries the resolved scope's `company_id`, stamped at
/// creation and checked on every read. A caller-supplied
/// `company_id` is never trusted on its own - it is only ever
/// passed as the `requested` argument to `tenant_scope`, which
/// rejects a mismatch with 403.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

/// Tenant scope for a tenant-row handler, resolved through the
/// entitlements module's `TenantScope` - the same pattern that
/// module uses (`entitlements/tenant.rs`: the tenant context is
/// the only trusted tenant identity, a `requested` company that
/// differs from it is a 403, and a context with no company id is
/// refused with 403 `tenant.required`). Handlers pass a
/// caller-supplied `company_id` here as `requested` so a tenant
/// can never file or list under another company's id; they pass
/// `None` when the request names no company.
pub(crate) fn tenant_scope(req: &HttpRequest, requested: Option<String>) -> UnifiedResult<TenantScope> {
    TenantScope::resolve(req, requested)
}

/// A row belongs to the resolved tenant only when its stored
/// `company_id` equals the scope's company. Anything else -
/// another tenant's row, or a row with no company at all - is
/// answered 404, never 403: the caller must not learn the row
/// exists. Rows created before tenant stamping carry no company
/// and are therefore unreachable through tenant-scoped
/// endpoints (fail closed).
pub(crate) fn require_company_scope(company_id: &Option<String>, scope: &TenantScope) -> UnifiedResult<()> {
    if company_id.as_deref() == Some(scope.company_id.as_str()) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Row not found"))
    }
}

/// Same rule for a report row.
pub(crate) fn require_report_scope(report: &ExpenseReport, scope: &TenantScope) -> UnifiedResult<()> {
    require_company_scope(&report.company_id, scope)
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is bound for the Diesel persistence layer (see
    // `resolve_tenant`); per-record tenant filtering is applied
    // by the handlers through `tenant_scope` /
    // `require_company_scope`, which stamp and check the scope's
    // company on every tenant row.
    let _tenant = resolve_tenant(req)?;
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

pub(crate) fn is_manager(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_EXPENSES_MANAGE)
}

/// Employees may act on their own rows; managers may act on any row.
pub(crate) fn require_row_access(
    ctx: &AppContext,
    caller: &Caller,
    owner_id: &str,
) -> UnifiedResult<()> {
    if is_manager(ctx, &caller.role) || caller.actor == owner_id {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Only the owner or a manager may access this row",
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one - never a constant in this file.
pub(crate) fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

pub(crate) fn require_currency(value: &str) -> UnifiedResult<String> {
    let checked = require_text(value, "currency", MAX_CURRENCY_LEN)?;
    if checked.len() != 3 || !checked.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(checked.to_ascii_uppercase())
}

pub(crate) fn require_amount(value: i64, field: &str) -> UnifiedResult<i64> {
    if value < 0 || value > MAX_AMOUNT_MINOR {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and {MAX_AMOUNT_MINOR}"
        )));
    }
    Ok(value)
}

pub(crate) fn optional_amount(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        Some(raw) => Ok(Some(require_amount(raw, field)?)),
        None => Ok(None),
    }
}

pub(crate) fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date as YYYY-MM-DD")))
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

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Allowed report status transitions. Anything not listed is
/// rejected with a 409 carrying the allowed list, so a report can
/// never jump between states outside this table. Note that
/// `submitted -> approved` and `submitted -> rejected` are only
/// ever performed by the approval-decision event listener, and
/// `approved -> reimbursed` only by the reimbursement-paid event
/// listener - no HTTP handler performs those moves.
pub(crate) fn allowed_report_transitions(from: &str) -> &'static [&'static str] {
    match from {
        REPORT_DRAFT => &[REPORT_SUBMITTED],
        REPORT_SUBMITTED => &[REPORT_DRAFT, REPORT_APPROVED, REPORT_REJECTED],
        REPORT_APPROVED => &[REPORT_REIMBURSED],
        REPORT_REJECTED => &[REPORT_DRAFT],
        REPORT_REIMBURSED => &[],
        _ => &[],
    }
}

pub(crate) fn transition_report(report: &mut ExpenseReport, to: &str) -> UnifiedResult<()> {
    if !allowed_report_transitions(&report.status).contains(&to) {
        return Err(UnifiedError::conflict(format!(
            "Cannot move report from \"{}\" to \"{}\"",
            report.status, to
        ))
        .with_metadata(
            "allowed_transitions",
            json!(allowed_report_transitions(&report.status)),
        ));
    }
    report.status = to.to_string();
    report.updated_at = Utc::now();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_report() -> ExpenseReport {
        let now = chrono::Utc::now();
        ExpenseReport {
            id: uuid::Uuid::now_v7(),
            employee_id: "emp-1".to_string(),
            company_id: None,
            policy_id: None,
            title: "Trip".to_string(),
            notes: None,
            status: "draft".to_string(),
            currency: "USD".to_string(),
            total_minor: 0,
            violations: vec![],
            approval_request_id: None,
            generic_approval_request_id: None,
            submitted_at: None,
            decided_at: None,
            decided_by: None,
            decision_reason: None,
            reimbursement_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn require_currency_normalizes_to_uppercase_iso_code() {
        assert_eq!(require_currency("usd").unwrap(), "USD");
        assert_eq!(require_currency("EUR").unwrap(), "EUR");
        assert_eq!(require_currency("  gbp ").unwrap(), "GBP");
    }

    #[test]
    fn require_currency_rejects_non_three_letter_codes() {
        assert!(require_currency("").is_err());
        assert!(require_currency("US").is_err());
        assert!(require_currency("USDX").is_err());
        assert!(require_currency("US1").is_err());
        assert!(require_currency("U D").is_err());
    }

    #[test]
    fn require_amount_enforces_zero_to_max_bounds() {
        assert_eq!(require_amount(0, "amount").unwrap(), 0);
        assert_eq!(require_amount(1_000_000_000_000, "amount").unwrap(), 1_000_000_000_000);
        assert!(require_amount(-1, "amount").is_err());
        assert!(require_amount(1_000_000_000_001, "amount").is_err());
        assert_eq!(optional_amount(None, "amount").unwrap(), None);
        assert_eq!(optional_amount(Some(5), "amount").unwrap(), Some(5));
        assert!(optional_amount(Some(-1), "amount").is_err());
    }

    #[test]
    fn allowed_report_transitions_match_the_state_table() {
        assert_eq!(allowed_report_transitions("draft"), ["submitted"]);
        assert_eq!(
            allowed_report_transitions("submitted"),
            ["draft", "approved", "rejected"]
        );
        assert_eq!(allowed_report_transitions("approved"), ["reimbursed"]);
        assert_eq!(allowed_report_transitions("rejected"), ["draft"]);
        assert_eq!(allowed_report_transitions("reimbursed"), [] as [&str; 0]);
        assert_eq!(allowed_report_transitions("bogus"), [] as [&str; 0]);
    }

    #[test]
    fn transition_report_moves_only_along_allowed_edges() {
        let mut report = draft_report();
        assert!(transition_report(&mut report, "submitted").is_ok());
        assert_eq!(report.status, "submitted");
        assert!(transition_report(&mut report, "approved").is_ok());
        assert_eq!(report.status, "approved");
        assert!(transition_report(&mut report, "draft").is_err());
        assert_eq!(report.status, "approved");
        assert!(transition_report(&mut report, "reimbursed").is_ok());
        assert!(transition_report(&mut report, "draft").is_err());

        let mut rejected = draft_report();
        rejected.status = "rejected".to_string();
        assert!(transition_report(&mut rejected, "draft").is_ok());
        assert!(transition_report(&mut rejected, "approved").is_err());
    }

    fn scoped_request(company_id: &str) -> HttpRequest {
        use actix_web::test::TestRequest;
        use unified_core::middleware::{TenantContext as ResolvedTenantContext, TenantKind};
        let req = TestRequest::default().to_http_request();
        req.extensions_mut().insert(ResolvedTenantContext {
            tenant_id: Uuid::now_v7().to_string(),
            company_id: company_id.to_string(),
            site_id: "site-1".to_string(),
            kind: TenantKind::Subdomain,
            host: "co-1.example.test".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        });
        req
    }

    fn scope_for(company_id: &str) -> TenantScope {
        TenantScope {
            tenant_id: Uuid::now_v7().to_string(),
            schema_name: "tenant_test".to_string(),
            company_id: company_id.to_string(),
            site_id: "site-1".to_string(),
        }
    }

    #[test]
    fn tenant_scope_resolves_the_context_company() {
        let req = scoped_request("co-1");
        let scope = tenant_scope(&req, None).unwrap();
        assert_eq!(scope.company_id, "co-1");
    }

    #[test]
    fn tenant_scope_accepts_a_matching_requested_company() {
        let req = scoped_request("co-1");
        assert!(tenant_scope(&req, Some("co-1".to_string())).is_ok());
    }

    #[test]
    fn tenant_scope_rejects_a_different_requested_company() {
        let req = scoped_request("co-1");
        let error = tenant_scope(&req, Some("co-2".to_string())).unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn tenant_scope_refuses_a_context_without_a_company() {
        let req = scoped_request("");
        let error = tenant_scope(&req, None).unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn tenant_scope_refuses_a_request_without_tenant_context() {
        let req = actix_web::test::TestRequest::default().to_http_request();
        let error = tenant_scope(&req, None).unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn require_report_scope_allows_own_company_and_404s_others() {
        let scope = scope_for("co-1");
        let mut own = draft_report();
        own.company_id = Some("co-1".to_string());
        assert!(require_report_scope(&own, &scope).is_ok());
        let mut other = draft_report();
        other.company_id = Some("co-2".to_string());
        let error = require_report_scope(&other, &scope).unwrap_err();
        assert_eq!(error.code, 404);
        let mut unstamped = draft_report();
        unstamped.company_id = None;
        let error = require_report_scope(&unstamped, &scope).unwrap_err();
        assert_eq!(error.code, 404);
    }

    #[test]
    fn require_company_scope_matches_the_same_rule() {
        let scope = scope_for("co-1");
        assert!(require_company_scope(&Some("co-1".to_string()), &scope).is_ok());
        assert_eq!(
            require_company_scope(&Some("co-2".to_string()), &scope)
                .unwrap_err()
                .code,
            404
        );
        assert_eq!(
            require_company_scope(&None, &scope).unwrap_err().code,
            404
        );
    }
}
