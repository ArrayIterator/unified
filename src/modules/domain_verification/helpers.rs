// Domain Verification — Helpers: identity, capabilities, templates, tokens.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Helpers: identity, capabilities, templates, tokens.
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the properties module.
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
/// (same fail-closed shape as the properties module).
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
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
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

/// Resolve the caller's company for a WRITE (C3 task 315, strict by
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


/// Verify a request body's `company_id` (`Option<Uuid>`) against the
/// caller's company scope and return the company id to store. Fails
/// closed (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3 task 315):
/// a write requires a resolved company scope. A caller scope that is not a UUID is refused the
/// same way (fail closed: token company ids are UUIDs).
pub(crate) fn require_company_match_uuid(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    let caller = caller_company(tenant, req)?;
    match (body_company_id, &caller) {
        (Some(body), Some(caller_company)) if body.to_string() != *caller_company => {
            Err(UnifiedError::forbidden(
                "company_id does not match the caller's company scope",
            ))
        }
        (Some(body), Some(_)) => Ok(Some(*body)),
        (Some(_), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller_company)) => {
            let id = Uuid::parse_str(caller_company).map_err(|_| {
                UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                )
            })?;
            Ok(Some(id))
        }
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// Verify a body `tenant_id` against the resolved tenant: a body
/// tenant other than the request's resolved tenant is refused (403).
/// The stored value is the body's own (an omitted tenant stays
/// omitted); this module's ownership rule only ever compares, it
/// never adopts a tenant.
pub(crate) fn require_body_tenant_match(
    tenant: &unified_core::tenancy::TenantContext,
    body_tenant_id: &Option<Uuid>,
) -> UnifiedResult<()> {
    match body_tenant_id {
        Some(body) if *body != tenant.tenant_id => Err(UnifiedError::forbidden(
            "tenant_id does not match the request's resolved tenant",
        )),
        _ => Ok(()),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3 task 320, rule A).
pub(crate) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.map(|id| id.to_string()).as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<Uuid>,
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
/// company scope FOR A WRITE (C3 task 315, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records — global (unscoped) records are platform-only
/// — and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) => match Uuid::parse_str(caller) {
            Ok(scope) if *record == scope => Ok(()),
            _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
        },
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Normalise and validate a domain name to its ASCII (punycode) form.
pub(crate) fn normalize_domain(raw: &str) -> UnifiedResult<String> {
    let canonical = DnsVerifier::normalize_domain_input(raw)?;
    let syntax = DnsVerifier::check_domain_syntax(&canonical);
    if !syntax.valid {
        return Err(UnifiedError::bad_request(format!(
            "Invalid domain: {}",
            syntax.issues.join("; ")
        )));
    }
    Ok(syntax.punycode.unwrap_or(canonical))
}

/// Render the lookup host from the admin host template. A template
/// containing `{domain}` is substituted verbatim; a bare label is
/// looked up as `<label>.<domain>`.
pub(crate) fn render_host(host_template: &str, domain: &str) -> String {
    if host_template.contains("{domain}") {
        host_template.replace("{domain}", domain)
    } else {
        let label = host_template.trim().trim_end_matches('.');
        if label.is_empty() {
            domain.to_string()
        } else {
            format!("{label}.{domain}")
        }
    }
}

/// Render the expected record value from the admin value template.
pub(crate) fn render_value(value_template: &str, token: &str) -> String {
    value_template.replace("{token}", token)
}

/// Generate a cryptographically random token from the admin alphabet
/// and length (OS RNG via `rand`).
pub(crate) fn generate_token(settings: &VerificationSettings) -> String {
    let mut rng = rand::rng();
    let alphabet: Vec<char> = settings.token_alphabet.chars().collect();
    (0..settings.token_length)
        .map(|_| {
            let index = rng.random_range(0..alphabet.len());
            alphabet[index]
        })
        .collect()
}

/// SHA-256 hex digest of a token. Only the digest is ever stored.
pub(crate) fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// What a verify outcome may report as the "expected value
/// prefix": only the fixed, non-secret literal from the admin
/// value template that comes before `{token}` (for example
/// `"unified-verify="` from `"unified-verify={token}"`), or the
/// record host when the template starts with the token. It never
/// contains characters of the token itself: the old behaviour of
/// returning the first 12 characters of the rendered value leaked
/// the start of the token whenever the template had no literal
/// prefix (for example a CNAME target of `"{token}.verify…"`).
pub(crate) fn value_prefix(value_template: &str, host: &str) -> String {
    if let Some(index) = value_template.find("{token}") {
        let literal = &value_template[..index];
        if !literal.is_empty() {
            return literal.to_string();
        }
    }
    host.to_string()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

/// The current (latest, non-superseded, non-revoked) token for a
/// domain, optionally scoped to one alias. A token is "current" when
/// it is the newest issued token for the domain/alias that has not
/// been superseded by a newer issuance or revoked.
pub(crate) fn current_token_for(domain: &str, alias_id: Option<Uuid>) -> Option<VerificationToken> {
    let mut candidates: Vec<VerificationToken> = tokens_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|token| token.domain == domain)
        .filter(|token| alias_id.map_or(true, |alias| token.alias_id == Some(alias)))
        .filter(|token| token.status != "superseded" && token.status != "revoked")
        .collect();
    candidates.sort_by(|a, b| b.issued_at.cmp(&a.issued_at));
    candidates.into_iter().next()
}

/// Status of a domain from its CURRENT token only. Never returns
/// `verified` unless a successful DNS check is recorded for the
/// current token (a verified token whose successor was issued, or an
/// expired token, does not count).
pub(crate) fn domain_status(domain: &str) -> (String, Option<DateTime<Utc>>) {
    let Some(token) = current_token_for(domain, None) else {
        return (STATUS_UNKNOWN.to_string(), None);
    };
    let now = Utc::now();
    if token.status == STATUS_VERIFIED {
        if let Some(verified_at) = token.verified_at {
            return (STATUS_VERIFIED.to_string(), Some(verified_at));
        }
    }
    if token.status == STATUS_PENDING && token.expires_at <= now {
        return (STATUS_FAILED.to_string(), None);
    }
    (token.status.clone(), token.verified_at)
}

/// Attempts recorded for a domain inside the last `window_seconds`.
pub(crate) fn attempts_in_window(domain: &str, window_seconds: i64) -> usize {
    let cutoff = Utc::now() - chrono::Duration::seconds(window_seconds);
    attempts_store()
        .iter()
        .filter(|entry| entry.value().domain == domain && entry.value().checked_at >= cutoff)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(
                uuid::Uuid::now_v7(),
                Vec::new(),
            )
            .with_company(company.to_string()),
            None => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
        };
        req.extensions_mut().insert(verified);
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
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_then_verified_caller() {
        let company = Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some("other-company"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(&Uuid::now_v7().to_string(), Some(&Uuid::now_v7().to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_shows_global_records_to_scoped_callers() {
        let scope = Some("acme".to_string());
        let acme = Uuid::now_v7();
        assert!(in_company_scope(&Some(acme), &Some(acme.to_string())));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &None));
    }

    #[test]
    fn require_company_match_uuid_refuses_writes_from_unresolved_callers() {
        let company = Uuid::now_v7();
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_company_match_uuid(&tenant, &req, &Some(company)).is_err());
        assert!(require_company_match_uuid(&tenant, &req, &None).is_err());
        let req = tenant_request(&company.to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            require_company_match_uuid(&tenant, &req, &Some(company)).unwrap(),
            Some(company)
        );
        assert_eq!(
            require_company_match_uuid(&tenant, &req, &None).unwrap(),
            Some(company)
        );
        assert!(require_company_match_uuid(&tenant, &req, &Some(Uuid::now_v7())).is_err());
    }

    #[test]
    fn require_body_tenant_match_refuses_a_foreign_tenant() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_body_tenant_match(&tenant, &None).is_ok());
        assert!(require_body_tenant_match(&tenant, &Some(tenant.tenant_id)).is_ok());
        assert!(require_body_tenant_match(&tenant, &Some(Uuid::now_v7())).is_err());
    }

    #[test]
    fn render_host_substitutes_domain_placeholder() {
        assert_eq!(
            render_host("_verify.{domain}", "example.com"),
            "_verify.example.com"
        );
    }

    #[test]
    fn render_host_appends_bare_label() {
        assert_eq!(
            render_host("_unified-verify", "example.com"),
            "_unified-verify.example.com"
        );
    }

    #[test]
    fn render_host_empty_label_returns_domain() {
        assert_eq!(render_host("", "example.com"), "example.com");
    }

    #[test]
    fn render_value_substitutes_token() {
        assert_eq!(
            render_value("unified-verify={token}", "abc123"),
            "unified-verify=abc123"
        );
    }

    #[test]
    fn hash_token_empty_input_is_known_sha256() {
        assert_eq!(
            hash_token(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hash_token_is_deterministic_and_distinct() {
        assert_eq!(hash_token("same"), hash_token("same"));
        assert_ne!(hash_token("one"), hash_token("two"));
        assert_eq!(hash_token("x").len(), 64);
    }

    #[test]
    fn value_prefix_returns_the_template_literal_before_the_token() {
        assert_eq!(
            value_prefix("unified-verify={token}", "_verify.example.com"),
            "unified-verify="
        );
        assert_eq!(
            value_prefix("prefix-{token}-suffix", "_verify.example.com"),
            "prefix-"
        );
    }

    #[test]
    fn value_prefix_falls_back_to_the_host_when_the_template_starts_with_the_token() {
        assert_eq!(
            value_prefix("{token}.verify.example.net", "_verify.example.com"),
            "_verify.example.com"
        );
    }

    #[test]
    fn value_prefix_never_leaks_token_characters() {
        let token = "AbC123xYzSecret";
        let template = "unified-verify={token}";
        let rendered = render_value(template, token);
        assert!(rendered.contains(token));
        let prefix = value_prefix(template, "_verify.example.com");
        assert_eq!(prefix, "unified-verify=");
        assert!(!prefix.contains(token));
        assert!(!prefix.contains("AbC123"));
        // A template with no literal prefix reports the host, not
        // the start of the rendered (token-bearing) value.
        let cname_prefix = value_prefix("{token}.verify.example.net", "_verify.example.com");
        assert_eq!(cname_prefix, "_verify.example.com");
        assert!(!cname_prefix.contains(token));
    }

    #[test]
    fn attempts_in_window_counts_only_recent_attempts_for_the_domain() {
        let domain = format!("count-{}.example.com", Uuid::now_v7());
        let now = Utc::now();
        let attempt = |checked_at: DateTime<Utc>| VerificationAttempt {
            id: Uuid::now_v7(),
            token_id: Uuid::now_v7(),
            domain: domain.clone(),
            alias_id: None,
            attempt_number: 1,
            result: STATUS_PENDING.to_string(),
            record_found: None,
            looked_up: json!({}),
            error: None,
            checked_at,
        };
        attempts_store().insert(Uuid::now_v7(), attempt(now));
        attempts_store().insert(Uuid::now_v7(), attempt(now - chrono::Duration::seconds(10)));
        // Outside the 60s window: must not count.
        attempts_store().insert(
            Uuid::now_v7(),
            attempt(now - chrono::Duration::seconds(3600)),
        );
        // Another domain: must not count.
        let mut other = attempt(now);
        other.domain = format!("other-{}.example.com", Uuid::now_v7());
        attempts_store().insert(other.id, other);
        assert_eq!(attempts_in_window(&domain, 60), 2);
    }

    #[test]
    fn generate_token_uses_configured_length_and_alphabet() {
        let settings = default_settings();
        let token = generate_token(&settings);
        assert_eq!(token.chars().count(), settings.token_length);
        assert!(token.chars().all(|c| settings.token_alphabet.contains(c)));
    }
}
