// cards — Internal service helpers shared by the handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Caller identity and capability checks (same shape as the other
// modules: `X-User-Id` identifies the actor, `X-Role` carries the
// role slug the capability check runs against, `role.check`
// answers; no answer means not allowed — fail closed).
// ---------------------------------------------------------------------------

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

fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group A).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry a `tenant_id`, not a company field,
/// so the guard is a fail-closed gate plus tenant partitioning in
/// the handlers (custom rules and counts are already filtered by
/// tenant id); no per-record company check is applied. Real
/// per-company partitioning needs a `company_id` on the records,
/// a C11 follow-up.
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
        (None, verified) => Ok(verified.clone()),
    }
}

/// Fail closed (403) when no company scope resolves at all. Writes
/// in this gate-only module require a resolved company: the records
/// carry no company field to stamp, so there is no global-row write
/// to fall back to (the shared `CallerScope::Unresolved` rule).
pub(crate) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
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
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_company_accepts_a_resolved_company() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_company(&tenant, &req).unwrap(), "acme");
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

// ---------------------------------------------------------------------------
// Hook answerer.
// ---------------------------------------------------------------------------

/// Answer `cards.validate_requested`: validate the number from the
/// event payload (never stored), set the result on the context, and
/// emit `cards.validated` with the masked PAN and the result. The
/// payload must carry `tenant_id`; without it the request is
/// refused outright (fail closed, no validation, no emit). A
/// number-less payload is answered only in modes `"method"` and
/// `"claim"` (stored method / claimed brand + expiry); mode
/// `"full"` — and any unknown or absent mode — still requires the
/// number, so a number-less payload is never accepted as a fully
/// validated new card.
pub(crate) fn on_validate_requested(state: Arc<CardsState>, ctx: &mut HookContext) {
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if tenant_id.is_empty() {
        ctx.set(
            "result",
            json!({
                "valid": false,
                "matched_rule": "none",
                "masked_pan": "",
                "reason_codes": ["missing_tenant_context"],
                "is_test_number": false,
            }),
        );
        ctx.set(
            "error",
            json!("missing tenant_id: validation refused without a tenant context"),
        );
        return;
    }
    let mode = ctx
        .get("mode")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("full");
    let expiry = ctx
        .get("expiry")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let cvv = ctx
        .get("cvv")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let raw_number = ctx.get("number").and_then(|value| value.as_str());
    // Same bound as the HTTP handler (`MAX_RAW_NUMBER_LENGTH`):
    // a caller reaching validation through the hook bus must not
    // be able to submit an arbitrarily long number. Refused
    // before any validation or recording, with an empty masked
    // PAN so the over-long input is never echoed back.
    if let Some(raw) = raw_number {
        if raw.len() > MAX_RAW_NUMBER_LENGTH {
            ctx.set(
                "result",
                json!({
                    "valid": false,
                    "matched_rule": "none",
                    "masked_pan": "",
                    "reason_codes": ["invalid_length"],
                    "is_test_number": false,
                    "inactive_brand": false,
                }),
            );
            ctx.set(
                "error",
                json!("number is too long (at most 64 characters)"),
            );
            return;
        }
    }
    let number = raw_number
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let result = match number {
        Some(number) => {
            let input = ValidateInput { number, expiry, cvv };
            validate_and_record(&state, &tenant_id, &input)
        }
        None => {
            // Number-less payload: only the stored-method and
            // claim modes may proceed, and only with the brand +
            // expiry checks. Mode `full` (and any unknown mode, or
            // a payload with no mode at all) still requires the
            // number — a number-less payload is never accepted as
            // a fully validated new card.
            if mode != "method" && mode != "claim" {
                ctx.set(
                    "result",
                    json!({
                        "valid": false,
                        "matched_rule": "none",
                        "masked_pan": "",
                        "reason_codes": ["invalid_format"],
                        "is_test_number": false,
                    }),
                );
                ctx.set("error", json!("missing number in cards.validate_requested"));
                return;
            }
            let claimed_brand = ctx
                .get("claimed_brand")
                .and_then(|value| value.as_str())
                .or_else(|| ctx.get("brand").and_then(|value| value.as_str()));
            let masked_pan = numberless_masked_pan(ctx);
            validate_numberless_and_record(
                &state,
                &tenant_id,
                claimed_brand,
                expiry.as_deref(),
                masked_pan,
            )
        }
    };
    let payload = serde_json::to_value(&result).unwrap_or_else(|_| {
        json!({
            "valid": false,
            "matched_rule": "none",
            "masked_pan": "",
            "reason_codes": ["invalid_format"],
            "is_test_number": false,
            "inactive_brand": false,
        })
    });
    ctx.set("result", payload.clone());
    ctx.add_result(payload);
    if let Some(bus) = state.hooks.read().expect("hooks lock poisoned").clone() {
        bus.emit(
            CARDS_VALIDATED,
            json!({
                "tenant_id": tenant_id,
                "masked_pan": result.masked_pan,
                "brand_id": result.brand_id,
                "brand_name": result.brand_name,
                "matched_rule": result.matched_rule,
                "valid": result.valid,
                "reason_codes": result.reason_codes,
                "is_test_number": result.is_test_number,
                "inactive_brand": result.inactive_brand,
            }),
        );
    }
}
