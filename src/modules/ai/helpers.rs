//! ai module - Caller, permissions, text checks, usage and company scope.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::constants::EVENT_USER_COMPANY_REQUESTED;
use super::context::estimate_cost_usd;
use super::providers::RawCompletion;
use super::state::AiState;
use super::types::AiProviderConnection;
use super::types::AiUsageRecord;
use super::types::RateBucket;

// ---------------------------------------------------------------------------
// Helpers: caller, permissions, validation.
// ---------------------------------------------------------------------------

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
///
/// Public so owning modules can build a `Caller` and hand it to
/// `run_ai_capability` from their own AI endpoints.
#[derive(Debug, Clone)]
pub struct Caller {
    pub actor: String,
    pub role: String,
}

pub fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// (Same fail-closed shape as the tickets module's capability check.)
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

/// Public so owning modules can enforce `ai:use` on their own AI
/// endpoints before delegating to `run_ai_capability`.
pub fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3, task 314, group C).
//
// AI records carry companies as `Uuid`: policies carry a required
// `company_id`, usage records carry `company_id: Option<Uuid>`.
// Provider connections, drafts, personal keys / MCP servers and the
// shared context carry no company field and are gate-only (drafts
// and personal records are additionally user-scoped by owner).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller()` or any capability
/// check.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in, as a `Uuid`: the tenant
/// context's company first, the verified caller's server-resolved
/// company otherwise. Fails closed (403) when both are present and
/// disagree, and when either present value is not a parseable UUID.
/// The `X-Company-Id` header is NEVER trusted here: only the
/// verified caller (company resolved from the database at
/// authentication time) and the middleware-resolved tenant context
/// are sources. Do NOT use `resolve_caller_company` (header-first,
/// task 317's job) for guard decisions.
pub(crate) fn caller_company_uuid(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<Uuid>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    let tenant_company = match &tenant.company_id {
        Some(raw) => Some(Uuid::parse_str(raw).map_err(|_| {
            UnifiedError::forbidden("Tenant company scope is not a valid company id")
        })?),
        None => None,
    };
    let verified_company = match &verified {
        Some(raw) => Some(Uuid::parse_str(raw).map_err(|_| {
            UnifiedError::forbidden("Verified caller company is not a valid company id")
        })?),
        None => None,
    };
    match (tenant_company, verified_company) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company)),
        (None, verified_company) => Ok(verified_company),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. Shared rule (`CallerScope::can_access_record`): a company
/// caller reads its own rows plus global (unscoped) rows; an
/// unresolved caller reads global rows only.
pub(crate) fn in_company_scope_uuid(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<Uuid>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none() || record_company_id.as_ref() == Some(company),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope_uuid` as a filter instead.
pub(crate) fn require_record_in_company_scope_uuid(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<Uuid>,
) -> UnifiedResult<()> {
    if in_company_scope_uuid(record_company_id, caller_company) {
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

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let company = Uuid::now_v7();
        let req = tenant_request(&company.to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company_uuid(&tenant, &req).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn caller_company_uuid_prefers_verified_caller_when_tenant_has_none() {
        let company = Uuid::now_v7();
        let req = tenant_request("", Some(&company.to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company_uuid(&tenant, &req).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn caller_company_uuid_rejects_verified_caller_mismatch() {
        let req = tenant_request(&Uuid::now_v7().to_string(), Some(&Uuid::now_v7().to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company_uuid(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_uuid_ignores_the_x_company_id_header() {
        // A forged header must never choose the company: only the
        // tenant context and the verified caller are sources.
        let req = TestRequest::default()
            .insert_header(("X-Company-Id", Uuid::now_v7().to_string()))
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
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company_uuid(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_uuid_follows_the_shared_rule() {
        let company = Uuid::now_v7();
        let scope = Some(company);
        // A company caller reads its own rows plus global rows.
        assert!(in_company_scope_uuid(&Some(company), &scope));
        assert!(in_company_scope_uuid(&None, &scope));
        assert!(!in_company_scope_uuid(&Some(Uuid::now_v7()), &scope));
        // An unresolved caller reads global rows only.
        assert!(in_company_scope_uuid(&None, &None));
        assert!(!in_company_scope_uuid(&Some(company), &None));
    }

    #[test]
    fn require_company_scope_denies_unresolved_callers() {
        // The task-312 rule, pinned here too: a platform admin with
        // capability `ai:manage_providers` and no company gets 403
        // on AI policy management until the task-315 platform
        // decision exists. Do NOT re-open this hole.
        use crate::modules::ai::validation::require_company_scope;
        let company = Uuid::now_v7();
        assert!(require_company_scope(Some(company), company).is_ok());
        assert!(require_company_scope(Some(Uuid::now_v7()), company).is_err());
        assert!(require_company_scope(None, company).is_err());
    }
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

pub(crate) fn optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

/// Emit an audit-shaped event on the hook bus: the audit module
/// reads `actor_id`, `resource`, and `id` from the payload. The
/// audit module only records events listed in its `AUDITED_EVENTS`,
/// so the integrator must add the `ai.*` names there.
pub(crate) fn emit_audit(ctx: &AppContext, event: &str, actor: &str, resource: &str, extra: Value) {
    let mut payload = match extra {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    payload.insert("actor_id".to_string(), Value::String(actor.to_string()));
    payload.insert("resource".to_string(), Value::String(resource.to_string()));
    ctx.hooks.emit(event, Value::Object(payload));
}

// ---------------------------------------------------------------------------
// Helpers: rate limits and usage recording.
// ---------------------------------------------------------------------------

/// Enforce the per-minute rate limit for one provider connection
/// and one caller. The bucket is keyed by `(connection, user)` so
/// one user cannot exhaust the shared per-minute budget for
/// everyone on that connection; callers whose actor id is not a
/// UUID share one anonymous bucket per connection. Returns a 429
/// with `retry_after_secs` in metadata when the minute window is
/// exhausted.
pub(crate) fn check_rate_limit(
    state: &AiState,
    connection: &AiProviderConnection,
    user_id: Option<Uuid>,
) -> UnifiedResult<()> {
    let limit = connection.rate_limit_per_minute;
    if limit == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut bucket = state
        .rate_buckets
        .entry((connection.id, user_id))
        .or_insert(RateBucket {
            window_start: now,
            count: 0,
        });
    if now.duration_since(bucket.window_start) >= Duration::from_secs(60) {
        bucket.window_start = now;
        bucket.count = 0;
    }
    if bucket.count >= limit {
        let retry_after = 60 - now.duration_since(bucket.window_start).as_secs().min(60);
        return Err(UnifiedError::new(
            format!(
                "Rate limit exceeded for provider \"{}\": at most {limit} calls per minute",
                connection.name
            ),
            429,
        )
        .with_metadata("provider_id", Value::String(connection.id.to_string()))
        .with_metadata("retry_after_secs", Value::from(retry_after)));
    }
    bucket.count += 1;
    Ok(())
}

/// Record one provider call's usage. `fallback_prompt_tokens` is
/// the prompt-token estimate for the call (0 when unknown): when
/// the provider reports no token counts, the record falls back to
/// that estimate instead of counting 0, so the call still
/// accumulates against token and cost caps. The insert runs under
/// the cap ledger lock so a cap check cannot read monthly usage
/// while a usage row is being written.
pub(crate) fn record_usage(
    state: &AiState,
    connection: &AiProviderConnection,
    actor: &str,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    use_case: &str,
    completion: Option<&RawCompletion>,
    latency_ms: u64,
    error_code: Option<u16>,
    used_personal_key: bool,
    fallback_prompt_tokens: u32,
) -> AiUsageRecord {
    let (mut prompt_tokens, completion_tokens) = match completion {
        Some(raw) => (raw.prompt_tokens, raw.completion_tokens),
        None => (None, None),
    };
    // No reported counts: fall back to the prompt-token estimate
    // for cap accounting. Without this, providers that omit
    // `usage` (notably local endpoints) would never accumulate
    // against token or cost caps.
    if prompt_tokens.is_none() && fallback_prompt_tokens > 0 {
        prompt_tokens = Some(fallback_prompt_tokens);
    }
    let total_tokens = match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => Some(prompt + completion),
        (Some(prompt), None) => Some(prompt),
        (None, Some(completion)) => Some(completion),
        (None, None) => None,
    };
    let estimated_cost_usd = match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => {
            estimate_cost_usd(&connection.model, prompt, completion)
        }
        (Some(prompt), None) => estimate_cost_usd(&connection.model, prompt, 0),
        _ => None,
    };
    let record = AiUsageRecord {
        id: Uuid::now_v7(),
        provider_id: connection.id,
        provider_kind: connection.kind.as_str().to_string(),
        actor: actor.to_string(),
        user_id,
        company_id,
        use_case: use_case.to_string(),
        model: connection.model.clone(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd,
        latency_ms,
        status: if error_code.is_none() {
            "success".to_string()
        } else {
            "error".to_string()
        },
        error_code,
        denial_reason: None,
        used_personal_key,
        created_at: Utc::now(),
    };
    // Serialize the insert against cap checks (see
    // `check_caps_and_reserve`): a check must not read monthly
    // usage while this row is being written.
    let _ledger = state
        .cap_ledger_lock
        .lock()
        .expect("ai cap ledger lock poisoned");
    state.usage.insert(record.id, record.clone());
    record
}

/// Record a call the allowlist gate denied. There is no provider
/// connection for a denied call, so the record carries the requested
/// provider/model (or the zero UUID / `"unknown"` when none was
/// resolved) and the non-leaking reason code. Token counts stay
/// zero: a denied call never consumed anything.
pub(crate) fn record_denied_usage(
    state: &AiState,
    actor: &str,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    use_case: &str,
    model: &str,
    provider_id: Uuid,
    provider_kind: &str,
    reason: &str,
) -> AiUsageRecord {
    let record = AiUsageRecord {
        id: Uuid::now_v7(),
        provider_id,
        provider_kind: provider_kind.to_string(),
        actor: actor.to_string(),
        user_id,
        company_id,
        use_case: use_case.to_string(),
        model: model.to_string(),
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        estimated_cost_usd: None,
        latency_ms: 0,
        status: "error".to_string(),
        error_code: Some(403),
        denial_reason: Some(reason.to_string()),
        used_personal_key: false,
        created_at: Utc::now(),
    };
    state.usage.insert(record.id, record.clone());
    record
}

// ---------------------------------------------------------------------------
// Access allowlists: caller identity, validation, authorization.
//
// Enforcement model
// -----------------
// The allowlist is default-deny. Enforcement is switched on
// unless an admin explicitly opts into legacy open mode
// (`AiGlobalSettings::open_mode`, changed through
// `PATCH /gating/settings`) AND no policy exists anywhere (see
// `enforcement_on` in `gating.rs`). With open mode on and no
// policies, every caller holding `ai:use` may call, and usage is
// still recorded with whatever user/company ids resolve. The
// moment the first company or user policy exists, enforcement is
// strict and default-deny: a company or user without an enabled
// policy is refused. Create the company policy BEFORE the first
// user policy, or the users of that company are locked out
// immediately.
//
// Company membership is resolved server-side: the verified
// caller's server-resolved company first, with a fallback to the
// `user.company_requested` hook answered by the users module.
// The `X-Company-Id` header is NEVER trusted here: it is
// client-asserted, so trusting it would let any JWT caller claim
// another company's AI caps and policies.
// ---------------------------------------------------------------------------

/// The caller's user id, parsed from the `X-User-Id` header. The
/// actor string is the only identity this module trusts; a body
/// field is never accepted as the user id. `None` when the actor id
/// is not a UUID (legacy / non-user actors).
pub(crate) fn caller_user_id(caller: &Caller) -> Option<Uuid> {
    Uuid::parse_str(&caller.actor).ok()
}

/// The caller's company id: the verified caller's server-resolved
/// company first, then the `user.company_requested` hook. `None`
/// when neither resolves; callers without a company are platform
/// admins. The `X-Company-Id` header is NEVER read here: it is
/// client-asserted, and trusting it would let any JWT caller claim
/// another company's AI caps and policies.
///
/// Public so owning modules can resolve the company id to pass to
/// `run_ai_capability` from their own AI endpoints.
pub fn resolve_caller_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> Option<Uuid> {
    if let Some(raw) = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty())
    {
        if let Ok(id) = Uuid::parse_str(raw.trim()) {
            return Some(id);
        }
    }
    let hook = ctx.hooks.emit(
        EVENT_USER_COMPANY_REQUESTED,
        json!({ "user_id": caller.actor }),
    );
    hook.get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

#[cfg(test)]
mod tests {
    use super::check_rate_limit;
    use super::record_usage;
    use crate::modules::ai::AiProviderConnection;
    use crate::modules::ai::AiProviderKind;
    use crate::modules::ai::providers::RawCompletion;
    use crate::modules::ai::state::AiState;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_connection(rate_limit: u32) -> AiProviderConnection {
        let now = Utc::now();
        AiProviderConnection {
            id: Uuid::now_v7(),
            name: "test".to_string(),
            kind: AiProviderKind::Local,
            base_url: "https://8.8.8.8/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key_encrypted: None,
            key_fingerprint: None,
            enabled: true,
            rate_limit_per_minute: rate_limit,
            allow_private_host: false,
            allow_http: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn rate_limit_bucket_is_per_connection_and_user() {
        let state = AiState::default();
        let connection = test_connection(2);
        let user_a = Some(Uuid::now_v7());
        let user_b = Some(Uuid::now_v7());
        assert!(check_rate_limit(&state, &connection, user_a).is_ok());
        assert!(check_rate_limit(&state, &connection, user_a).is_ok());
        // User A is exhausted, but user B still has a full budget
        // on the same connection.
        assert!(check_rate_limit(&state, &connection, user_a).is_err());
        assert!(check_rate_limit(&state, &connection, user_b).is_ok());
        assert!(check_rate_limit(&state, &connection, user_b).is_ok());
        assert!(check_rate_limit(&state, &connection, user_b).is_err());
        // Anonymous callers share one bucket per connection.
        assert!(check_rate_limit(&state, &connection, None).is_ok());
    }

    #[test]
    fn usage_without_reported_counts_falls_back_to_prompt_estimate() {
        let state = AiState::default();
        let connection = test_connection(0);
        let user_id = Uuid::now_v7();
        let raw = RawCompletion {
            text: "answer".to_string(),
            prompt_tokens: None,
            completion_tokens: None,
            model: "gpt-4o-mini".to_string(),
        };
        let record = record_usage(
            &state,
            &connection,
            "actor",
            Some(user_id),
            None,
            "complete",
            Some(&raw),
            10,
            None,
            false,
            100,
        );
        // gpt-4o-mini prompt price is 0.15 USD per million tokens.
        assert_eq!(record.prompt_tokens, Some(100));
        assert_eq!(record.total_tokens, Some(100));
        assert!(record.estimated_cost_usd.is_some());
        let cost = record.estimated_cost_usd.unwrap();
        assert!((cost - 0.000_015).abs() < 1e-9, "{cost} != 0.000015");
    }

    #[test]
    fn usage_without_counts_and_without_estimate_still_counts_zero() {
        let state = AiState::default();
        let connection = test_connection(0);
        let record = record_usage(
            &state,
            &connection,
            "actor",
            None,
            None,
            "test",
            None,
            10,
            None,
            false,
            0,
        );
        assert_eq!(record.total_tokens, None);
        assert_eq!(record.estimated_cost_usd, None);
    }
}
