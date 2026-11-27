//! Capability, rate-limit, pagination and run-recording helpers.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::HttpRequest;
use chrono::{Utc};
use serde::{Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::types::{NetworkToolRun, network_tool_events};
use super::store::{db_insert_run, db_pool, rate_limits_store, runs_store};

// Viewer identity and capability checks.
//
// Identity comes from the request headers (`X-User-Id` / `X-Role`),
// exactly like the risk, content, forms, and appointments modules.
// Query parameters are never read for identity: they are logged,
// cached, and bookmarked, so a claimed role in the query string must
// not decide what the caller may do. `company_id` is the one
// exception and it is not identity: it is a list filter / run
// attribution read through `company_filter`, and every read is
// already scoped to the caller's own records unless they hold the
// manage capability.

/// Viewer identity from the request headers (`X-User-Id` /
/// `X-Role`). Returns (viewer id, viewer role); either side may be
/// absent, and every downstream check fails closed on an absent
/// side: no role means no capability, and no viewer id means no
/// ownership.
pub(crate) fn viewer_of(req: &HttpRequest) -> (Option<String>, Option<String>) {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        (
            Some(verified.user_id.clone()),
            verified.roles.first().cloned(),
        )
    } else {
        (None, None)
    }
}

/// The `company_id` list filter / run attribution from the query
/// parameters. This is a filter, not caller identity: it never
/// decides what the caller may do.
pub(crate) fn company_filter(params: &PaginationParams) -> Option<String> {
    params
        .filters
        .get("company_id")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Parse a company id from a query/body string. A value that is not
/// a UUID is treated as absent rather than failing the request: the
/// company filter is advisory, the capability check is authoritative.
pub(crate) fn parse_company_id(value: Option<String>) -> Option<Uuid> {
    value.and_then(|value| Uuid::parse_str(value.trim()).ok())
}

/// Capabilities granted to each built-in role. `admin` and
/// `super_admin` hold everything; `operator` and `developer` may run
/// and manage tools but may not probe private targets; `viewer` and
/// `auditor` are read-only. Unknown or absent roles hold nothing.
pub(crate) fn role_capabilities(role: &str) -> &'static [&'static str] {
    match role {
        "admin" | "super_admin" => &[
            "network_tools:view",
            "network_tools:run",
            "network_tools:manage",
            "network_tools:probe_private",
        ],
        "operator" | "developer" => &[
            "network_tools:view",
            "network_tools:run",
            "network_tools:manage",
        ],
        "viewer" | "auditor" => &["network_tools:view"],
        _ => &[],
    }
}

pub(crate) fn has_capability(role: Option<&str>, capability: &str) -> bool {
    match role {
        Some(role) => role_capabilities(role).contains(&capability),
        None => false,
    }
}

/// Environment variable that enables probing private / loopback /
/// link-local targets. Server-side operator config only: no request
/// field can enable it. Disabled unless the variable is set to a
/// truthy value (`1`, `true`, `yes`, `on`). A config-file field
/// should replace this once one exists; the variable is the gate
/// until then.
pub(crate) const ALLOW_PRIVATE_ENV: &str = "UNIFIED_NETWORK_TOOLS_ALLOW_PRIVATE";

/// True when private-target probing is allowed: the operator must
/// have enabled it server-side (`UNIFIED_NETWORK_TOOLS_ALLOW_PRIVATE`)
/// AND the caller's role must grant the `probe_private` capability.
/// The capability alone is not enough: the role is self-asserted
/// until verified-caller auth lands (C1), so the server-side switch
/// is the real gate, and it defaults to off.
pub(crate) fn allow_private_probes(viewer_role: Option<&str>) -> bool {
    if !has_capability(viewer_role, "network_tools:probe_private") {
        return false;
    }
    std::env::var(ALLOW_PRIVATE_ENV)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// A caller may touch a record they own, or any record when they
/// hold the manage capability. Anything else is forbidden.
pub(crate) fn require_owner_or_manage(
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
    owner: &str,
) -> UnifiedResult<()> {
    if viewer_id == Some(owner) || has_capability(viewer_role, "network_tools:manage") {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "A record can only be read or changed by its owner or a manager",
        ))
    }
}

pub(crate) fn require_capability(role: Option<&str>, capability: &str) -> UnifiedResult<()> {
    if has_capability(role, capability) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {capability}"
        )))
    }
}

// Rate limiting (in-memory, per actor, fixed 60-second windows).

pub(crate) fn check_rate_limit(actor: &str, scope: &str, limit: u64) -> UnifiedResult<()> {
    let key = format!("{actor}:{scope}");
    let now = Instant::now();
    let mut entry = rate_limits_store().entry(key).or_insert((now, 0));
    if entry.0.elapsed() >= Duration::from_secs(60) {
        *entry = (now, 1);
        return Ok(());
    }
    entry.1 += 1;
    if entry.1 > limit {
        return Err(UnifiedError::new(
            format!("Rate limit exceeded for {scope}: at most {limit} requests per minute"),
            429,
        ));
    }
    Ok(())
}

// Run recording: every tool run is stored and a hook event is emitted.
// The stored summary never contains a response body.

pub(crate) fn record_run(
    ctx: &AppContext,
    actor: &str,
    company_id: Option<&str>,
    tool: &str,
    target: &str,
    started: Instant,
    outcome: &str,
    summary: Value,
) -> Uuid {
    let run = NetworkToolRun {
        id: Uuid::new_v4(),
        user_id: actor.to_string(),
        company_id: company_id.and_then(|value| Uuid::parse_str(value).ok()),
        tool: tool.to_string(),
        target: target.chars().take(512).collect(),
        duration_ms: started.elapsed().as_millis() as i64,
        outcome: outcome.to_string(),
        summary,
        created_at: Utc::now(),
    };
    runs_store().insert(run.id, run.clone());
    if let Some(pool) = db_pool(ctx) {
        let _ = db_insert_run(&pool, &run);
    }
    ctx.hooks.emit(
        network_tool_events::RUN,
        json!({
            "run_id": run.id,
            "tool": tool,
            "target": run.target,
            "actor": actor,
            "duration_ms": run.duration_ms,
            "outcome": outcome,
        }),
    );
    run.id
}

pub(crate) fn notify(ctx: &AppContext, user_id: &str, title: String, body: String, kind: &str) {
    // Direct `notification.requested`, same pattern as the
    // certificates module's `ssl.expiring` alerts: the notifications
    // module does not subscribe to these events, so without this
    // request the alert would land nowhere.
    ctx.hooks.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "title": title,
            "body": body,
            "kind": kind,
        }),
    );
}

// Pagination helper (same shape as the licensing module's `paginate`).

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination: Pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
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

/// Actor identity from the request headers (`X-User-Id` /
/// `X-Role`). See `viewer_of`: query parameters are never read for
/// identity. The actor falls back to `anonymous` only as a
/// rate-limit and audit label; it grants nothing, because every
/// capability check fails closed on an absent role.
pub(crate) fn tool_actor(req: &HttpRequest) -> (String, Option<String>) {
    let (viewer_id, viewer_role) = viewer_of(req);
    (
        viewer_id.unwrap_or_else(|| "anonymous".to_string()),
        viewer_role,
    )
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
    use actix_web::test::TestRequest;

    #[test]
    fn viewer_identity_comes_from_the_verified_caller_not_query_params() {
        let req = verified_request("user-1", "admin");
        assert_eq!(
            viewer_of(&req),
            (
                Some(test_user_uuid("user-1").to_string()),
                Some("admin".to_string())
            )
        );
        assert_eq!(
            tool_actor(&req),
            (
                test_user_uuid("user-1").to_string(),
                Some("admin".to_string())
            )
        );
        // A role claimed only in the query string is ignored:
        // query params are logged, cached, and bookmarked.
        let req = TestRequest::default()
            .uri("/api/v1/network-tools/ping?viewer_id=spoofed&viewer_role=admin")
            .to_http_request();
        assert_eq!(viewer_of(&req), (None, None));
        assert_eq!(tool_actor(&req), ("anonymous".to_string(), None));
    }

    #[test]
    fn forged_admin_query_param_grants_no_capability() {
        let req = TestRequest::default()
            .uri("/api/v1/network-tools/ping?viewer_id=spoofed&viewer_role=admin")
            .to_http_request();
        let (actor, viewer_role) = tool_actor(&req);
        assert_eq!(actor, "anonymous");
        assert!(!has_capability(viewer_role.as_deref(), "network_tools:run"));
        assert!(!has_capability(
            viewer_role.as_deref(),
            "network_tools:probe_private"
        ));
        assert!(
            require_capability(viewer_role.as_deref(), "network_tools:run").is_err()
        );
        assert!(
            require_capability(viewer_role.as_deref(), "network_tools:manage").is_err()
        );
    }
}
