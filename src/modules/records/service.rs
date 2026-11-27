// records - Internal service helpers shared by the handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

/// Retention in days from the `records` config section
/// (`AppConfig.records.retention_days`), falling back to
/// `DEFAULT_RETENTION_DAYS` when the configured value is zero.
pub(crate) fn retention_days(ctx: &AppContext) -> u32 {
    let configured = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .records
        .retention_days;
    if configured == 0 {
        DEFAULT_RETENTION_DAYS
    } else {
        configured
    }
}

/// True when an explicit retention setting exists in the `records`
/// config section. This must read the raw configured value, not
/// `retention_days()`: that helper falls back to
/// `DEFAULT_RETENTION_DAYS` and so never returns zero. A raw zero
/// only happens for a hand-built config that skipped validation
/// (validation rejects zero), so zero means "not configured" and
/// the default applies.
pub(crate) fn retention_configured(ctx: &AppContext) -> bool {
    ctx.config
        .read()
        .expect("config lock poisoned")
        .records
        .retention_days
        != 0
}

// Caller identity and capability checks (same shape as the cards and
// search modules: `X-User-Id` identifies the actor, `X-Role` carries
// the role slug the capability check runs against, `role.check`
// answers; no answer means not allowed - fail closed).

struct Caller {
    /// Captured for audit parity with the other modules; the records
    /// handlers only need the role, so this is not read here yet.
    #[allow(dead_code)]
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

// Shared list / export filters. Both handlers parse the same
// `PaginationParams` through these helpers so the export can never
// drift away from the list filters again.

/// The parsed list/export filters: exact-match fields plus an
/// inclusive `from` / `to` window on `created_at` and a free-text
/// search applied to the action, actor, entity, and summary.
pub(crate) struct RecordFilters {
    pub(crate) actor: Option<String>,
    pub(crate) action: Option<String>,
    pub(crate) entity_type: Option<String>,
    pub(crate) entity_id: Option<String>,
    pub(crate) from: Option<DateTime<Utc>>,
    pub(crate) to: Option<DateTime<Utc>>,
    pub(crate) search: Option<String>,
}

/// Parse the shared filters out of the query params. A malformed
/// `from` / `to` value is a 400: the bound must be an RFC 3339
/// date-time.
pub(crate) fn parse_record_filters(
    params: &PaginationParams,
) -> UnifiedResult<RecordFilters> {
    let parse_bound = |name: &str| -> UnifiedResult<Option<DateTime<Utc>>> {
        match params.filters.get(name) {
            Some(raw) => raw.parse::<DateTime<Utc>>().map(Some).map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Filter \"{name}\" must be an RFC 3339 date-time, got \"{raw}\""
                ))
            }),
            None => Ok(None),
        }
    };
    Ok(RecordFilters {
        actor: params.filters.get("actor").cloned(),
        action: params.filters.get("action").cloned(),
        entity_type: params.filters.get("entity_type").cloned(),
        entity_id: params.filters.get("entity_id").cloned(),
        from: parse_bound("from")?,
        to: parse_bound("to")?,
        search: params.search.clone().map(|value| value.to_lowercase()),
    })
}

/// True when `record` passes every filter in `filters`.
pub(crate) fn record_matches(record: &SystemRecord, filters: &RecordFilters) -> bool {
    if let Some(actor) = &filters.actor {
        if record.actor != *actor {
            return false;
        }
    }
    if let Some(action) = &filters.action {
        if record.action != *action {
            return false;
        }
    }
    if let Some(entity_type) = &filters.entity_type {
        if record.entity_type != *entity_type {
            return false;
        }
    }
    if let Some(entity_id) = &filters.entity_id {
        if record.entity_id != *entity_id {
            return false;
        }
    }
    if let Some(from) = filters.from {
        if record.created_at < from {
            return false;
        }
    }
    if let Some(to) = filters.to {
        if record.created_at > to {
            return false;
        }
    }
    if let Some(needle) = &filters.search {
        let haystack = format!(
            "{} {} {} {} {}",
            record.action, record.actor, record.entity_type, record.entity_id, record.summary
        )
        .to_lowercase();
        if !haystack.contains(needle.as_str()) {
            return false;
        }
    }
    true
}

/// Derive the entity type and id for a record from the event payload.
///
/// Precedence: explicit `entity_type` / `entity_id` keys first, then
/// a `resource` value of the form `type:id`, then an `id` key paired
/// with the event name prefix (`user.created` -> `user`), and finally
/// the whole event name as the type with an empty id.
pub(crate) fn derive_entity(event: &str, data: &Value, resource: Option<&str>) -> (String, String) {
    if let (Some(entity_type), Some(entity_id)) = (
        data.get("entity_type").and_then(|value| value.as_str()),
        data.get("entity_id").and_then(|value| value.as_str()),
    ) {
        return (entity_type.to_string(), entity_id.to_string());
    }
    if let Some(entity_type) = data.get("entity_type").and_then(|value| value.as_str()) {
        let entity_id = data
            .get("entity_id")
            .and_then(|value| value.as_str())
            .or_else(|| data.get("id").and_then(|value| value.as_str()))
            .unwrap_or("")
            .to_string();
        return (entity_type.to_string(), entity_id);
    }
    if let Some(resource) = resource {
        if let Some((kind, id)) = resource.split_once(':') {
            if !kind.is_empty() && !id.is_empty() {
                return (kind.to_string(), id.to_string());
            }
        }
        return (event.to_string(), resource.to_string());
    }
    let entity_type = event.split('.').next().unwrap_or(event).to_string();
    let entity_id = data
        .get("id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            data.get("order_id")
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            data.get("module_id")
                .and_then(|value| value.as_str())
        })
        .unwrap_or("")
        .to_string();
    (entity_type, entity_id)
}

pub(crate) fn count_by(
    records: &[SystemRecord],
    key_fn: impl Fn(&SystemRecord) -> &str,
) -> Vec<CountEntry> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for record in records {
        *counts.entry(key_fn(record).to_string()).or_insert(0) += 1;
    }
    let mut entries: Vec<CountEntry> = counts
        .into_iter()
        .map(|(key, count)| CountEntry { key, count })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
    entries
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

    use super::super::types::{PERM_RECORDS_MANAGE, PERM_RECORDS_READ};

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn context_with_retention(days: u32) -> AppContext {
        let mut config = AppConfig::default();
        config.records.retention_days = days;
        AppContext::new(
            config,
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

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn record_at(
        id: &str,
        actor: &str,
        action: &str,
        summary: &str,
        created_at: DateTime<Utc>,
    ) -> SystemRecord {
        SystemRecord {
            id: id.to_string(),
            seq: 0,
            actor: actor.to_string(),
            action: action.to_string(),
            entity_type: "task".to_string(),
            entity_id: "e1".to_string(),
            summary: summary.to_string(),
            details: Value::Null,
            ip: None,
            created_at,
        }
    }

    fn params_with(filters: &[(&str, &str)], search: Option<&str>) -> PaginationParams {
        PaginationParams {
            search: search.map(str::to_string),
            filters: filters
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn record(action: &str) -> SystemRecord {
        SystemRecord {
            id: Uuid::new_v4().to_string(),
            seq: 0,
            actor: "tester".to_string(),
            action: action.to_string(),
            entity_type: "task".to_string(),
            entity_id: "e1".to_string(),
            summary: String::new(),
            details: Value::Null,
            ip: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn derive_entity_prefers_explicit_keys() {
        let data = serde_json::json!({"entity_type": "task", "entity_id": "42"});
        assert_eq!(
            derive_entity("task.created", &data, None),
            ("task".to_string(), "42".to_string())
        );
    }

    #[test]
    fn derive_entity_type_without_id_falls_back_to_id_key() {
        let data = serde_json::json!({"entity_type": "user", "id": "7"});
        assert_eq!(
            derive_entity("user.updated", &data, None),
            ("user".to_string(), "7".to_string())
        );
    }

    #[test]
    fn derive_entity_falls_back_to_resource() {
        let data = serde_json::json!({});
        assert_eq!(
            derive_entity("user.updated", &data, Some("user:7")),
            ("user".to_string(), "7".to_string())
        );
    }

    #[test]
    fn derive_entity_resource_without_colon_uses_event_name() {
        let data = serde_json::json!({});
        assert_eq!(
            derive_entity("order.created", &data, Some("weird")),
            ("order.created".to_string(), "weird".to_string())
        );
    }

    #[test]
    fn derive_entity_falls_back_to_event_prefix_and_id() {
        let data = serde_json::json!({"id": "99"});
        assert_eq!(
            derive_entity("user.deleted", &data, None),
            ("user".to_string(), "99".to_string())
        );
    }

    #[test]
    fn derive_entity_empty_when_nothing_known() {
        let data = serde_json::json!({});
        assert_eq!(
            derive_entity("geo.lookup", &data, None),
            ("geo".to_string(), "".to_string())
        );
    }

    #[test]
    fn count_by_sorts_by_count_then_key() {
        let records = vec![record("a"), record("a"), record("b")];
        let counts = count_by(&records, |r| r.action.as_str());
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0].key, "a");
        assert_eq!(counts[0].count, 2);
        assert_eq!(counts[1].key, "b");
        assert_eq!(counts[1].count, 1);
    }

    #[test]
    fn count_by_ties_break_alphabetically() {
        let records = vec![record("b"), record("a")];
        let counts = count_by(&records, |r| r.action.as_str());
        assert_eq!(counts[0].key, "a");
        assert_eq!(counts[1].key, "b");
    }

    #[test]
    fn count_by_empty_input_is_empty() {
        let counts = count_by(&[], |r| r.action.as_str());
        assert!(counts.is_empty());
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let found = caller(&authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        let missing_role = TestRequest::default().to_http_request();
        assert!(caller(&missing_role).is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_RECORDS_READ));
        assert!(require_permission(&ctx, "admin", PERM_RECORDS_READ).is_err());
        assert!(require_permission(&ctx, "admin", PERM_RECORDS_MANAGE).is_err());
    }

    #[test]
    fn caller_and_permission_gate_accept_when_role_check_allows() {
        let ctx = allowing_context();
        let found = caller(&authed_request()).unwrap();
        assert_eq!(found.role, "admin");
        assert!(require_permission(&ctx, &found.role, PERM_RECORDS_MANAGE).is_ok());
        // Anonymous callers are still rejected even when the role
        // check would allow.
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
    }

    #[test]
    fn retention_configured_true_for_default_config() {
        let ctx = test_context();
        assert!(retention_configured(&ctx));
        assert_eq!(retention_days(&ctx), DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn retention_configured_false_when_raw_value_is_zero() {
        // A zero survives only in a hand-built config that skipped
        // validation; `retention_days()` still falls back to the
        // default, which is exactly why `configured` must read the
        // raw value instead of `retention_days(&ctx) != 0`.
        let ctx = context_with_retention(0);
        assert!(!retention_configured(&ctx));
        assert_eq!(retention_days(&ctx), DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn retention_configured_true_for_explicit_value() {
        let ctx = context_with_retention(30);
        assert!(retention_configured(&ctx));
        assert_eq!(retention_days(&ctx), 30);
    }

    #[test]
    fn record_matches_applies_from_and_to_window() {
        let old = record_at(
            "old",
            "alice",
            "task.created",
            "old",
            "2026-01-01T00:00:00Z".parse().unwrap(),
        );
        let mid = record_at(
            "mid",
            "alice",
            "task.created",
            "mid",
            "2026-03-15T00:00:00Z".parse().unwrap(),
        );
        let new = record_at(
            "new",
            "alice",
            "task.created",
            "new",
            "2026-06-01T00:00:00Z".parse().unwrap(),
        );
        let filters = parse_record_filters(&params_with(
            &[
                ("from", "2026-02-01T00:00:00Z"),
                ("to", "2026-04-01T00:00:00Z"),
            ],
            None,
        ))
        .unwrap();
        assert!(!record_matches(&old, &filters));
        assert!(record_matches(&mid, &filters));
        assert!(!record_matches(&new, &filters));
    }

    #[test]
    fn record_matches_applies_search_case_insensitively() {
        let hit = record_at(
            "hit",
            "alice",
            "task.created",
            "Alpha Report",
            "2026-03-15T00:00:00Z".parse().unwrap(),
        );
        let miss = record_at(
            "miss",
            "bob",
            "task.completed",
            "Beta Report",
            "2026-03-15T00:00:00Z".parse().unwrap(),
        );
        let filters = parse_record_filters(&params_with(&[], Some("ALPHA"))).unwrap();
        assert!(record_matches(&hit, &filters));
        assert!(!record_matches(&miss, &filters));
    }

    #[test]
    fn record_matches_keeps_exact_match_filters() {
        let record = record_at(
            "r",
            "alice",
            "task.created",
            "s",
            "2026-03-15T00:00:00Z".parse().unwrap(),
        );
        let matching = parse_record_filters(&params_with(
            &[("actor", "alice"), ("action", "task.created")],
            None,
        ))
        .unwrap();
        assert!(record_matches(&record, &matching));
        let other_actor =
            parse_record_filters(&params_with(&[("actor", "bob")], None)).unwrap();
        assert!(!record_matches(&record, &other_actor));
    }

    #[test]
    fn parse_record_filters_rejects_malformed_bounds() {
        assert!(parse_record_filters(&params_with(&[("from", "not-a-date")], None)).is_err());
        assert!(parse_record_filters(&params_with(&[("to", "2026-13-99")], None)).is_err());
        assert!(parse_record_filters(&params_with(&[], None)).is_ok());
    }
}
