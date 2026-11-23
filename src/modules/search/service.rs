// Global Search — Business logic shared by handlers and hooks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

pub(crate) fn tokenize(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

pub(crate) fn paginate_hits(
    mut hits: Vec<SearchHit>,
    params: &PaginationParams,
) -> PageResult<SearchHit> {
    let pagination: Pagination = params.normalized();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.document.updated_at.cmp(&a.document.updated_at))
            .then(a.document.title.cmp(&b.document.title))
    });
    let total = hits.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<SearchHit> = hits
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn group_counts(documents: &[SearchDocument]) -> Vec<KindGroup> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for document in documents {
        *counts.entry(document.kind.clone()).or_insert(0) += 1;
    }
    let mut groups: Vec<KindGroup> = counts
        .into_iter()
        .map(|(kind, count)| KindGroup { kind, count })
        .collect();
    groups.sort_by(|a, b| b.count.cmp(&a.count).then(a.kind.cmp(&b.kind)));
    groups
}

// ---------------------------------------------------------------------------
// Caller identity, capability checks, and safe URLs.
// ---------------------------------------------------------------------------

/// Capability required by the index-mutating endpoints (`POST
/// /index`, `POST /reindex`, `DELETE /{kind}/{id}`). Read endpoints
/// (search, suggest, stats) are public and need no capability. The
/// admin role holds this through the roles module wildcard; other
/// roles need an explicit grant.
pub(crate) const PERM_SEARCH_MANAGE: &str = "search.manage";

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

/// Resolve the caller and require `permission` in one step, exactly
/// like the cache module's `authorize`.
pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// True when `url` uses an allowlisted shape for indexed document
/// links: `http://`, `https://`, a site-relative path starting with
/// `/` (but not protocol-relative `//`), or a `#` fragment. Anything
/// else — `javascript:`, `data:`, `vbscript:`, a bare scheme, or a
/// URL containing control characters — is rejected, so a stored
/// document can never carry a link that executes on click.
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
    use chrono::{DateTime, Utc};

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn document(id: &str, kind: &str, title: &str, snippet: &str) -> SearchDocument {
        SearchDocument {
            id: id.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            snippet: snippet.to_string(),
            url: None,
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    fn hit(document: SearchDocument, score: i64) -> SearchHit {
        SearchHit { document, score }
    }

    #[test]
    fn tokenize_lowercases_and_splits_on_whitespace() {
        assert_eq!(tokenize("Hello  WORLD "), vec!["hello".to_string(), "world".to_string()]);
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
        assert_eq!(tokenize("one"), vec!["one".to_string()]);
    }

    #[test]
    fn paginate_hits_sorts_by_score_then_recency_then_title() {
        let older = SearchDocument {
            updated_at: DateTime::from_timestamp(1_600_000_000, 0).unwrap(),
            ..document("1", "task", "Alpha", "")
        };
        let newer = SearchDocument {
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            ..document("2", "task", "Beta", "")
        };
        let params = PaginationParams {
            page: Some(1),
            per_page: Some(10),
            sort: None,
            order: None,
            search: None,
            filters: std::collections::HashMap::new(),
        };
        let page = paginate_hits(vec![hit(older, 5), hit(newer, 5)], &params);
        assert_eq!(page.pagination.total, 2);
        assert_eq!(page.items[0].document.title, "Beta");
        assert_eq!(page.items[1].document.title, "Alpha");
        let top = paginate_hits(
            vec![hit(document("1", "task", "Low", ""), 1), hit(document("2", "task", "High", ""), 9)],
            &params,
        );
        assert_eq!(top.items[0].document.title, "High");
    }

    #[test]
    fn paginate_hits_pages_results() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(1),
            sort: None,
            order: None,
            search: None,
            filters: std::collections::HashMap::new(),
        };
        let page = paginate_hits(
            vec![hit(document("1", "task", "First", ""), 10), hit(document("2", "task", "Second", ""), 5)],
            &params,
        );
        assert_eq!(page.pagination.total, 2);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].document.title, "Second");
    }

    #[test]
    fn group_counts_sorts_by_count_then_kind() {
        let docs = vec![
            document("1", "task", "A", ""),
            document("2", "task", "B", ""),
            document("3", "invoice", "C", ""),
        ];
        let groups = group_counts(&docs);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].kind, "task");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[1].kind, "invoice");
        assert_eq!(groups[1].count, 1);
        assert!(group_counts(&[]).is_empty());
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_SEARCH_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_SEARCH_MANAGE).is_err());
        let req = verified_request("user-1", "admin");
        assert!(authorize(&ctx, &req, PERM_SEARCH_MANAGE).is_err());
    }

    #[test]
    fn is_safe_url_allowlists_http_https_relative_and_fragment_only() {
        assert!(is_safe_url("https://example.com/doc"));
        assert!(is_safe_url("http://example.com/doc"));
        assert!(is_safe_url("/tasks/1"));
        assert!(is_safe_url("#section"));
        assert!(!is_safe_url(""));
        assert!(!is_safe_url("javascript:alert(1)"));
        assert!(!is_safe_url("JaVaScRiPt:alert(1)"));
        assert!(!is_safe_url("data:text/html,hi"));
        assert!(!is_safe_url("//evil.example.com/x"));
        assert!(!is_safe_url("relative/path"));
    }
}
