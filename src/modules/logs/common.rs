// logs — common.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock, RwLock,
};
use uuid::Uuid;
use super::*;
use super::redaction::*;
use super::types::*;
use super::store::*;
use super::service::*;
use super::tracing::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

pub(crate) fn parse_time(value: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!("Invalid RFC 3339 timestamp: {value}"))
        })
}

/// Apply the shared list filters (level, source, kind, search, date
/// range) to the buffered entries, newest first by default sort.
pub(crate) fn filtered_entries(
    state: &LogsState,
    params: &PaginationParams,
) -> UnifiedResult<Vec<LogEntry>> {
    let from = match params.filters.get("from") {
        Some(value) => Some(parse_time(value)?),
        None => None,
    };
    let to = match params.filters.get("to") {
        Some(value) => Some(parse_time(value)?),
        None => None,
    };
    let sort = params.allowed_sort(&["timestamp", "level", "source"], "timestamp");
    let mut items: Vec<LogEntry> = state
        .entries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            params
                .filters
                .get("level")
                .is_none_or(|level| entry.level == *level)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|source| entry.source == *source)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| entry.kind == *kind)
                && params.filters.get("request_id").is_none_or(|id| {
                    entry.request_id.as_deref() == Some(id.as_str())
                })
                && params.filters.get("user_id").is_none_or(|id| {
                    entry.user_id.as_deref() == Some(id.as_str())
                })
                && from.is_none_or(|from| entry.timestamp >= from)
                && to.is_none_or(|to| entry.timestamp <= to)
                && matches_search(
                    &format!(
                        "{} {} {} {}",
                        entry.message,
                        entry.source,
                        entry.request_id.clone().unwrap_or_default(),
                        entry.path.clone().unwrap_or_default()
                    ),
                    params,
                )
        })
        .collect();
    match sort.as_str() {
        "level" => items.sort_by_key(|entry| level_rank(&entry.level)),
        "source" => items.sort_by(|a, b| a.source.cmp(&b.source)),
        _ => items.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.seq.cmp(&b.seq))),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Caller and capabilities (same pattern as todos/pricing: fail closed).
// ---------------------------------------------------------------------------

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
#[allow(dead_code)]
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

/// Resolve the caller and require `permission` in one step, for use
/// at the top of every handler.
pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<()> {
    let call = caller(req)?;
    require_permission(ctx, &call.role, permission)
}

// ---------------------------------------------------------------------------
// Ingestion limits for `record_log`.
// ---------------------------------------------------------------------------

/// Maximum message length accepted by `record_log`.
pub(crate) const MAX_LOG_MESSAGE_CHARS: usize = 10_000;

/// Maximum source length accepted by `record_log`.
pub(crate) const MAX_LOG_SOURCE_CHARS: usize = 128;

/// Maximum serialized size of the context object accepted by `record_log`.
pub(crate) const MAX_LOG_CONTEXT_BYTES: usize = 65_536;

pub(crate) fn check_record_log_limits(
    source: &str,
    message: &str,
    context: &Option<Value>,
) -> UnifiedResult<()> {
    if source.chars().count() > MAX_LOG_SOURCE_CHARS {
        return Err(UnifiedError::bad_request(format!(
            "source must be at most {MAX_LOG_SOURCE_CHARS} characters"
        )));
    }
    if message.chars().count() > MAX_LOG_MESSAGE_CHARS {
        return Err(UnifiedError::bad_request(format!(
            "message must be at most {MAX_LOG_MESSAGE_CHARS} characters"
        )));
    }
    if let Some(context) = context {
        let size = serde_json::to_string(context)
            .map(|text| text.len())
            .unwrap_or(usize::MAX);
        if size > MAX_LOG_CONTEXT_BYTES {
            return Err(UnifiedError::bad_request(format!(
                "context must be at most {MAX_LOG_CONTEXT_BYTES} bytes"
            )));
        }
    }
    Ok(())
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

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let call = caller(&req).unwrap();
        assert_eq!(call.actor, test_user_uuid("user-1").to_string());
        assert_eq!(call.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_LOGS_VIEW));
        assert!(!check_capability(&ctx, "admin", PERM_LOGS_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_LOGS_VIEW).is_err());
        let req = verified_request("user-1", "admin");
        assert!(authorize(&ctx, &req, PERM_LOGS_VIEW).is_err());
    }

    #[test]
    fn record_log_limits_reject_oversize_fields() {
        assert!(check_record_log_limits("app", "ok", &None).is_ok());
        assert!(check_record_log_limits(
            &"s".repeat(MAX_LOG_SOURCE_CHARS + 1),
            "ok",
            &None
        )
        .is_err());
        assert!(check_record_log_limits(
            "app",
            &"m".repeat(MAX_LOG_MESSAGE_CHARS + 1),
            &None
        )
        .is_err());
        let big_context = Some(json!({ "pad": "x".repeat(MAX_LOG_CONTEXT_BYTES) }));
        assert!(check_record_log_limits("app", "ok", &big_context).is_err());
        let small_context = Some(json!({ "key": "value" }));
        assert!(check_record_log_limits("app", "ok", &small_context).is_ok());
    }
}
