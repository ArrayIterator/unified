// Site Builder - Small shared helpers: caller identity, capability checks, and HTTP responses.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::validation::*;
use super::rendering::*;
use super::handlers::*;
use super::module::*;

pub(crate) fn slug_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").expect("slug regex must compile")
    })
}

/// Escape text for safe inclusion in HTML content and attributes.
pub(crate) fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Derive a URL slug from a title (`"Hello World!"` -> `"hello-world"`).
pub(crate) fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = true;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

// Server-side block rendering (theme engine contract)

pub(crate) fn content_str(content: &Value, key: &str) -> Option<String> {
    content
        .get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

pub(crate) fn content_bool(content: &Value, key: &str) -> bool {
    content.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

pub(crate) fn not_found_page(debug: bool) -> HttpResponse {
    api_json::error_response(&UnifiedError::not_found("Page not found"), debug)
}

// Caller identity and capability checks

/// Capabilities checked through the `role.check` hook; fails closed.
/// The admin role holds every capability through the roles module
/// wildcard; other roles need an explicit grant.
pub(crate) const PERM_SITE_BUILDER_VIEW: &str = "site-builder.view";

pub(crate) const PERM_SITE_BUILDER_MANAGE: &str = "site-builder.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events and community
/// modules.
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

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Package gate for the site builder: the builder is a
/// CHOICE, off by default. A request acting in a company scope gets
/// the builder only when that company's package includes the
/// `site-builder` module entry; the control plane (no company scope
/// resolves) is never gated, so the system home page keeps working.
/// The company is resolved like the saved-views caller scope: the
/// middleware tenant context first, the verified caller's
/// server-resolved company otherwise, failing closed when the two
/// disagree. The `X-Company-Id` header is NEVER trusted here.
pub(crate) fn require_site_builder_entitlement(
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<()> {
    let tenant = unified_core::tenancy::TenantContext::from_http_request(req).ok();
    let tenant_company = tenant.as_ref().and_then(|item| item.company_id.clone());
    let verified_company = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    let company = match (&tenant_company, &verified_company) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            return Err(UnifiedError::forbidden(
                "Tenant context company and verified caller company disagree",
            ));
        }
        (Some(tenant_company), _) => Some(tenant_company.clone()),
        (None, verified_company) => verified_company.clone(),
    };
    let Some(company) = company else {
        // No company scope: the control plane, never package-gated.
        return Ok(());
    };
    crate::modules::licensing::check_module_entitlement(ctx, &company, "site-builder")
}

/// Resolve the caller and require `permission` in one step, exactly
/// like the cache module's `authorize`. The site-builder package
/// gate runs after the capability check, so a tenant without the
/// package gets a 403 on every builder route, reads included.
pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    require_site_builder_entitlement(ctx, req)?;
    Ok(found)
}

// Safe URLs for rendered links and media

/// True when `url` uses an allowlisted shape for rendered links and
/// media: `http://`, `https://`, a site-relative path starting with
/// `/` (but not protocol-relative `//`), or a `#` fragment. Anything
/// else - `javascript:`, `data:`, `vbscript:`, a bare scheme, or a
/// URL containing control characters - is rejected, so an
/// HTML-escaped but dangerous URL can never reach an `href`, `src`,
/// or `action` attribute.
pub(crate) fn is_safe_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.chars().any(|c| c.is_control()) {
        return false;
    }
    if url.starts_with('#') {
        return true;
    }
    if url.starts_with('/') {
        return !url.starts_with("//");
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Return `url` when it passes [`is_safe_url`], otherwise `fallback`.
/// Renderers use this so an unsafe URL degrades to a harmless target
/// instead of reaching the page.
pub(crate) fn safe_url(url: &str, fallback: &str) -> String {
    if is_safe_url(url) {
        url.trim().to_string()
    } else {
        fallback.to_string()
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;
    use serde_json::json;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn escape_html_escapes_all_five_specials() {
        assert_eq!(escape_html("<a href=\"x\">&'</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&#x27;&lt;/a&gt;");
        assert_eq!(escape_html("plain text"), "plain text");
        assert_eq!(escape_html(""), "");
    }

    #[test]
    fn slugify_derives_lowercase_hyphenated_slugs() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  --Hi--There--  "), "hi-there");
        assert_eq!(slugify("123"), "123");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("About Us 2026"), "about-us-2026");
    }

    #[test]
    fn content_str_reads_string_fields_only() {
        let content = json!({"heading": "Hi", "count": 3});
        assert_eq!(content_str(&content, "heading").as_deref(), Some("Hi"));
        assert_eq!(content_str(&content, "missing"), None);
        assert_eq!(content_str(&content, "count"), None);
    }

    #[test]
    fn content_bool_defaults_to_false() {
        assert!(content_bool(&json!({"show": true}), "show"));
        assert!(!content_bool(&json!({"show": false}), "show"));
        assert!(!content_bool(&json!({}), "show"));
        assert!(!content_bool(&json!({"show": "yes"}), "show"));
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_SITE_BUILDER_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_SITE_BUILDER_MANAGE).is_err());
        let req = verified_request("user-1", "admin");
        assert!(authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE).is_err());
    }

    #[test]
    fn is_safe_url_allowlists_http_https_relative_and_fragment_only() {
        assert!(is_safe_url("https://cdn.example.com/a.png"));
        assert!(is_safe_url("http://example.com/page"));
        assert!(is_safe_url("HTTPS://EXAMPLE.COM/X"));
        assert!(is_safe_url("/go"));
        assert!(is_safe_url("/pages/about?x=1"));
        assert!(is_safe_url("#"));
        assert!(is_safe_url("#section-2"));
        assert!(is_safe_url("  https://example.com/trimmed  "));
        assert!(!is_safe_url(""));
        assert!(!is_safe_url("javascript:alert(1)"));
        assert!(!is_safe_url("JaVaScRiPt:alert(1)"));
        assert!(!is_safe_url("data:text/html,<script>alert(1)</script>"));
        assert!(!is_safe_url("vbscript:msgbox(1)"));
        assert!(!is_safe_url("//evil.example.com/x.png"));
        assert!(!is_safe_url("relative/path"));
        assert!(!is_safe_url("ftp://example.com/file"));
        assert!(!is_safe_url("javascript\n:alert(1)"));
        assert_eq!(safe_url("javascript:alert(1)", "#"), "#");
        assert_eq!(safe_url("/go", "#"), "/go");
    }
}

// Tenant edge guard (C3 pilot 4, group C).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Site builder (pages, sections, blocks) records carry no company field, so the guard is a
/// fail-closed gate only: real per-company partitioning needs a
/// `tenant_id` on the records (C11 follow-up), and no per-record
/// company check is applied here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
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
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
