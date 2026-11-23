//! Activity Timeline module. — Helpers.
//!
//! Split from `src/modules/timeline.rs`; behaviour unchanged.

use super::handlers::*;
use super::module::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Permission that grants read access to the timeline feed. The
/// timeline aggregates audit, records and task events, so the
/// audit view permission grants the same access.
pub(super) const PERM_TIMELINE_VIEW: &str = "timeline.view";
pub(super) const PERM_AUDIT_VIEW: &str = "audit.view";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the webhooks module.
pub(super) struct Caller {
    pub(super) actor: String,
    pub(super) role: String,
}

pub(super) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the webhooks capability check).
pub(super) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Require an authenticated caller who holds the timeline view
/// permission (or the audit view permission, which covers the same
/// audit data). Anonymous callers fail with 401 from `caller`;
/// authenticated callers without either permission fail with 403.
pub(super) fn authorize_timeline_view(
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    if check_capability(ctx, &caller.role, PERM_TIMELINE_VIEW)
        || check_capability(ctx, &caller.role, PERM_AUDIT_VIEW)
    {
        Ok(caller)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\" or \"{}\"",
            caller.role, PERM_TIMELINE_VIEW, PERM_AUDIT_VIEW
        )))
    }
}

/// Build the only payload the timeline ever stores or serves: a
/// whitelisted summary of an entry. Raw event payloads can carry
/// secrets, tokens, passwords, user ids and IPs, so they are never
/// stored in `TimelineEntry::details` and never served.
pub(super) fn whitelisted_details(
    summary: &str,
    entity_type: &str,
    entity_id: &str,
    actor: &str,
) -> Value {
    json!({
        "summary": summary,
        "entity_type": entity_type,
        "entity_id": entity_id,
        "actor": actor,
    })
}

/// Return a copy of `entry` whose `details` hold only the
/// whitelisted summary and whose `ip` is dropped, so list responses
/// can never leak a raw event payload or an IP even if an entry was
/// built with one.
pub(super) fn redact_entry(entry: TimelineEntry) -> TimelineEntry {
    let mut entry = entry;
    entry.details = whitelisted_details(
        &entry.summary,
        &entry.entity_type,
        &entry.entity_id,
        &entry.actor,
    );
    entry.ip = None;
    entry
}

pub(super) fn paginate<T: Serialize + Clone>(
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

pub(super) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Parse a `from` / `to` date filter (`YYYY-MM-DD` or RFC 3339).
/// Returns the error to report when the value does not parse.
///
/// A bare date parses to the start of that day, except for a `to`
/// bound (`end_of_day`): it parses to the last nanosecond of that
/// day (23:59:59.999999999), so a bare `to` date includes the whole
/// day and the range stays inclusive. RFC 3339 values are always
/// used exactly as given, for either bound.
pub(super) fn parse_date_filter(
    raw: &str,
    label: &str,
    end_of_day: bool,
) -> UnifiedResult<DateTime<Utc>> {
    if let Ok(parsed) = raw.parse::<DateTime<Utc>>() {
        return Ok(parsed);
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let naive = if end_of_day {
            date.and_hms_nano_opt(23, 59, 59, 999_999_999)
                .unwrap_or_default()
        } else {
            date.and_hms_opt(0, 0, 0).unwrap_or_default()
        };
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    Err(UnifiedError::bad_request(format!(
        "Filter {label} must be a date (YYYY-MM-DD) or an RFC 3339 timestamp"
    )))
}

pub(super) fn count_by(
    entries: &[TimelineEntry],
    key_fn: impl Fn(&TimelineEntry) -> String,
) -> Vec<CountEntry> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for entry in entries {
        *counts.entry(key_fn(entry)).or_insert(0) += 1;
    }
    let mut out: Vec<CountEntry> = counts
        .into_iter()
        .map(|(key, count)| CountEntry { key, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
    out
}

/// Shared filter step for the feed and the entity timeline: actor,
/// action, entity type/id, source, and an inclusive date range.
pub(super) fn apply_filters(
    mut items: Vec<TimelineEntry>,
    params: &PaginationParams,
) -> UnifiedResult<Vec<TimelineEntry>> {
    let from = match params.filters.get("from") {
        Some(raw) => Some(parse_date_filter(raw, "from", false)?),
        None => None,
    };
    let to = match params.filters.get("to") {
        Some(raw) => Some(parse_date_filter(raw, "to", true)?),
        None => None,
    };
    let date = params.filters.get("date").cloned();
    items.retain(|entry| {
        matches_search(
            &format!("{} {} {} {}", entry.action, entry.actor, entry.summary, entry.entity_id),
            params,
        ) && params
            .filters
            .get("actor")
            .is_none_or(|actor| entry.actor == *actor)
            && params.filters.get("action").is_none_or(|action| entry.action == *action)
            && params
                .filters
                .get("entity_type")
                .is_none_or(|entity_type| entry.entity_type == *entity_type)
            && params
                .filters
                .get("entity_id")
                .is_none_or(|entity_id| entry.entity_id == *entity_id)
            && params
                .filters
                .get("source")
                .is_none_or(|source| entry.source == *source)
            && from.is_none_or(|from| entry.created_at >= from)
            && to.is_none_or(|to| entry.created_at <= to)
            && date.as_ref().is_none_or(|date| {
                entry.created_at.format("%Y-%m-%d").to_string() == *date
            })
    });
    // Newest first by insertion sequence; `order=asc` reverses.
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    Ok(items)
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
    use std::sync::Arc;

    fn entry_at(created_at: &str) -> TimelineEntry {
        TimelineEntry {
            id: "test".to_string(),
            seq: 1,
            actor: "alice".to_string(),
            action: "task.created".to_string(),
            entity_type: "task".to_string(),
            entity_id: "7".to_string(),
            summary: "test entry".to_string(),
            source: "task".to_string(),
            details: json!({"password": "s3cr3t", "summary": "test entry"}),
            ip: Some("10.0.0.9".to_string()),
            created_at: created_at.parse::<DateTime<Utc>>().unwrap(),
        }
    }

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    /// A context whose `role.check` handler allows only `audit.view`.
    fn audit_only_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let allowed = context
                    .get("permission")
                    .and_then(|value| value.as_str())
                    == Some(PERM_AUDIT_VIEW);
                context.set("allowed", serde_json::Value::Bool(allowed));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        let error = caller(&req).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn authorize_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        let error = authorize_timeline_view(&ctx, &authed_request()).unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn authorize_rejects_anonymous_callers_with_401() {
        let ctx = allowing_context();
        let req = TestRequest::default().to_http_request();
        let error = authorize_timeline_view(&ctx, &req).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn authorize_accepts_timeline_view() {
        let ctx = allowing_context();
        let found = authorize_timeline_view(&ctx, &authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
    }

    #[test]
    fn authorize_accepts_audit_view_without_timeline_view() {
        let ctx = audit_only_context();
        let found = authorize_timeline_view(&ctx, &authed_request()).unwrap();
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn authorize_rejects_a_role_with_neither_view_permission() {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(false));
            }),
        );
        let error = authorize_timeline_view(&ctx, &authed_request()).unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn whitelisted_details_hold_only_the_summary_fields() {
        let details = whitelisted_details("did a thing", "order", "42", "alice");
        assert_eq!(details.as_object().unwrap().len(), 4);
        assert_eq!(details["summary"], "did a thing");
        assert_eq!(details["entity_type"], "order");
        assert_eq!(details["entity_id"], "42");
        assert_eq!(details["actor"], "alice");
        assert!(details.get("password").is_none());
        assert!(details.get("ip").is_none());
    }

    #[test]
    fn redact_entry_strips_raw_payloads_and_ips() {
        let redacted = redact_entry(entry_at("2026-01-15T12:00:00Z"));
        assert_eq!(redacted.details.as_object().unwrap().len(), 4);
        assert!(redacted.details.get("password").is_none());
        assert_eq!(redacted.details["summary"], "test entry");
        assert_eq!(redacted.details["entity_type"], "task");
        assert_eq!(redacted.details["entity_id"], "7");
        assert_eq!(redacted.details["actor"], "alice");
        assert_eq!(redacted.ip, None);
        // Every other field survives redaction unchanged.
        assert_eq!(redacted.id, "test");
        assert_eq!(redacted.action, "task.created");
        assert_eq!(redacted.source, "task");
    }

    #[test]
    fn paginate_slices_and_reports_totals() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
        let first = paginate(vec![1, 2, 3], &PaginationParams::default());
        assert_eq!(first.items, vec![1, 2, 3]);
        assert_eq!(first.pagination.total_pages, 1);
    }

    #[test]
    fn matches_search_rules() {
        let with_search = PaginationParams {
            search: Some("  ALICE ".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Alice in chains", &with_search));
        assert!(!matches_search("Bob", &with_search));
        assert!(matches_search("anything", &PaginationParams::default()));
        let blank = PaginationParams {
            search: Some("   ".to_string()),
            ..Default::default()
        };
        assert!(matches_search("anything", &blank));
    }

    #[test]
    fn parse_date_filter_accepts_dates_and_rfc3339() {
        let day = parse_date_filter("2026-01-15", "from", false).unwrap();
        assert_eq!(day.format("%Y-%m-%d").to_string(), "2026-01-15");
        assert_eq!(day.format("%H:%M:%S").to_string(), "00:00:00");
        let stamped = parse_date_filter("2026-01-15T10:30:00Z", "from", false).unwrap();
        assert_eq!(stamped.format("%H:%M:%S").to_string(), "10:30:00");
        assert!(parse_date_filter("not-a-date", "from", false).is_err());
        assert!(parse_date_filter("15/01/2026", "from", false).is_err());
        assert!(parse_date_filter("", "from", false).is_err());
    }

    #[test]
    fn parse_date_filter_to_bare_date_is_end_of_day() {
        // A bare `to` date must cover the whole day, not just midnight.
        let end = parse_date_filter("2026-01-15", "to", true).unwrap();
        assert_eq!(end.format("%Y-%m-%d").to_string(), "2026-01-15");
        assert_eq!(end.format("%H:%M:%S%.f").to_string(), "23:59:59.999999999");
        let start = parse_date_filter("2026-01-15", "to", false).unwrap();
        assert_eq!(start.format("%H:%M:%S").to_string(), "00:00:00");
        assert!(end > start);
    }

    #[test]
    fn parse_date_filter_rfc3339_is_exact_for_the_to_bound() {
        // An explicit timestamp is never shifted to end of day.
        let stamped = parse_date_filter("2026-01-15T10:30:00Z", "to", true).unwrap();
        assert_eq!(stamped.format("%H:%M:%S").to_string(), "10:30:00");
        let with_offset = parse_date_filter("2026-01-15T10:30:00+02:00", "to", true).unwrap();
        assert_eq!(with_offset.format("%H:%M:%S").to_string(), "08:30:00");
    }

    #[test]
    fn apply_filters_to_date_includes_the_whole_to_day() {
        let day_start = entry_at("2026-01-15T00:00:01Z");
        let day_end = entry_at("2026-01-15T23:59:59Z");
        let next_day = entry_at("2026-01-16T00:00:01Z");
        let entries = vec![day_start, day_end, next_day];
        let params = PaginationParams {
            filters: [
                ("from".to_string(), "2026-01-15".to_string()),
                ("to".to_string(), "2026-01-15".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let filtered = apply_filters(entries, &params).unwrap();
        assert_eq!(filtered.len(), 2);
        assert!(filtered
            .iter()
            .all(|entry| entry.created_at.format("%Y-%m-%d").to_string() == "2026-01-15"));
    }

    #[test]
    fn apply_filters_to_rfc3339_stays_exact() {
        let before = entry_at("2026-01-15T10:29:59Z");
        let after = entry_at("2026-01-15T10:30:01Z");
        let entries = vec![before, after];
        let params = PaginationParams {
            filters: [("to".to_string(), "2026-01-15T10:30:00Z".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let filtered = apply_filters(entries, &params).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].created_at.format("%H:%M:%S").to_string(),
            "10:29:59"
        );
    }

    #[test]
    fn count_by_orders_by_count_then_key() {
        let state = TimelineState::new();
        state.record("task.created", &json!({"actor_id": "alice"}));
        state.record("task.created", &json!({"actor_id": "alice"}));
        state.record("user.created", &json!({"actor_id": "bob"}));
        let entries = state.all_entries();
        let counts = count_by(&entries, |entry| entry.action.clone());
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0].key, "task.created");
        assert_eq!(counts[0].count, 2);
        assert_eq!(counts[1].key, "user.created");
        assert_eq!(counts[1].count, 1);
        assert!(count_by(&[], |entry| entry.action.clone()).is_empty());
    }

    #[test]
    fn apply_filters_actor_search_and_bad_date() {
        let state = TimelineState::new();
        state.record("task.created", &json!({"actor_id": "alice"}));
        state.record("user.created", &json!({"actor_id": "bob"}));
        let entries = state.all_entries();
        let by_actor = PaginationParams {
            filters: [("actor".to_string(), "bob".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let filtered = apply_filters(entries.clone(), &by_actor).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].actor, "bob");
        let by_search = PaginationParams {
            search: Some("alice".to_string()),
            ..Default::default()
        };
        assert_eq!(apply_filters(entries.clone(), &by_search).unwrap().len(), 1);
        let bad_date = PaginationParams {
            filters: [("from".to_string(), "not-a-date".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert!(apply_filters(entries, &bad_date).is_err());
    }

    #[test]
    fn apply_filters_desc_puts_newest_first() {
        let state = TimelineState::new();
        state.record("task.created", &json!({"actor_id": "alice"}));
        state.record("task.created", &json!({"actor_id": "bob"}));
        let entries = state.all_entries();
        let desc = PaginationParams {
            order: Some("desc".to_string()),
            ..Default::default()
        };
        let filtered = apply_filters(entries, &desc).unwrap();
        assert_eq!(filtered.len(), 2);
        assert!(filtered[0].seq > filtered[1].seq);
    }
}
