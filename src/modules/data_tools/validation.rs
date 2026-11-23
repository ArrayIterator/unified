// data_tools — Validation helpers and small pure utilities.
//
// Module-local, mirroring the documents module's shape: the caller
// is identified by the verified `X-User-Id` / `X-Role` headers and
// every route asks the roles module through the `role.check` hook.
// The check fails closed when the roles module does not answer.
// Data Tools rules are platform control-plane objects (like the
// Scheduler's jobs), so the gate is caller identity plus
// capability, not per-tenant scoping — no `require_tenant` call.
// Tenant-scoped rules are follow-up work.

use super::*;
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
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

/// The verified caller for one request: user id + role, resolved
/// server-side at authentication time. Fails closed (401) when no
/// verified caller is attached to the request.
pub(crate) struct Caller {
    pub actor: String,
    pub role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    Ok(Caller {
        actor: verified.user_id.clone(),
        role: verified.role().to_string(),
    })
}

/// Ask the roles module whether `role` holds `permission`, through
/// the `role.check` hook. Fail closed: no answer means not allowed.
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

/// Authorize one request: caller identity + one capability. Returns
/// the caller on success so handlers can use the actor id.
pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    if check_capability(ctx, &found.role, permission) {
        Ok(found)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{permission}\"",
            found.role
        )))
    }
}

/// Escape text for the admin HTML page: the page is rendered from
/// data the admin controls (rule descriptions), so every interpolated
/// value goes through here (the JSON API never needs it).
pub(crate) fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;
    use std::sync::Arc;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    #[test]
    fn authorize_rejects_anonymous_requests() {
        let req = TestRequest::default().to_http_request();
        let error = caller(&req).expect_err("anonymous request must fail");
        assert_eq!(error.code, 401);
    }

    #[test]
    fn check_capability_fails_closed_without_roles() {
        // A fresh AppContext has no roles module answering
        // `role.check`, so nothing is allowed.
        let ctx = AppContext::new(
            unified_core::config::AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        assert!(!check_capability(&ctx, "admin", PERM_ADMIN));
        assert!(!check_capability(&ctx, "admin", PERM_READ));
    }

    #[test]
    fn escape_html_neutralizes_markup() {
        assert_eq!(
            escape_html("<script>alert(\"x\")</script> & 'quoted'"),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;quoted&#39;"
        );
    }
}
