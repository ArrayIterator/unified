//! Settings - HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{types::*, module::*};

/// Merge settings contributed by another module into the settings
/// store (`settings.merge`, payload
/// `{ "namespace": "vcs.github", "settings": { "token_configured": true, ... } }`).
/// Only these namespaces are accepted (explicit allowlist):
/// `vcs.*` (emitted by the VCS module, `vcs/handlers_providers.rs`,
/// namespace `vcs.{provider}`), `auth.*` (emitted by the auth manager,
/// `auth_manager/handlers_providers.rs`, namespace `auth.{provider-id}`),
/// and the bare `cloudflare` namespace (emitted by Cloudflare,
/// `cloudflare/status_handlers.rs` via `state.settings_merge_payload()`).
/// Refused merges are logged instead of silently dropped, so a
/// misaddressed emit is visible in the logs; the result still reports
/// `merged: 0` with a reason, because the emitters never check it.
/// Only flag values are stored for `vcs.*` and `cloudflare`: those
/// modules never put a credential in the merge payload, so tokens
/// and webhook secrets stay in each module's own write-only store
/// and are never written here. Note the `auth.*` payloads are the
/// provider's stored settings map as emitted by the auth manager,
/// not flags-only, so the auth manager owns excluding secrets from
/// what it emits; this module stores what it receives. Merged values
/// are trusted internal flags, so they are stored without going
/// through the `config.validate` hook (that hook needs an
/// `AppContext`, which a synchronous hook handler does not have).
///
/// True when `namespace` is one of the namespaces `settings.merge`
/// accepts: the `vcs.*` and `auth.*` prefixes, or exactly
/// `cloudflare`.
pub(crate) fn is_mergeable_namespace(namespace: &str) -> bool {
    namespace.starts_with("vcs.")
        || namespace.starts_with("auth.")
        || namespace == "cloudflare"
}

pub(crate) fn on_settings_merge(store: SettingsStore, ctx: &mut HookContext) {
    let namespace = ctx
        .get("namespace")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if !is_mergeable_namespace(&namespace) {
        log::warn!(
            "settings.merge refused: namespace \"{namespace}\" is not mergeable (accepted: vcs.*, auth.*, cloudflare)"
        );
        ctx.add_result(json!({
            "merged": 0,
            "reason": "only vcs.*, auth.* and cloudflare namespaces can be merged",
        }));
        return;
    }
    let Some(settings) = ctx.get("settings").and_then(|value| value.as_object()) else {
        log::warn!(
            "settings.merge refused: namespace \"{namespace}\" carried no settings object"
        );
        ctx.add_result(json!({
            "merged": 0,
            "reason": "settings must be an object",
        }));
        return;
    };
    let mut merged = 0usize;
    for (key, value) in settings {
        if !valid_name(key) {
            continue;
        }
        let record = SettingRecord {
            namespace: namespace.clone(),
            key: key.clone(),
            value_type: infer_type(value).to_string(),
            value: value.clone(),
            updated_at: Utc::now(),
        };
        store.insert((namespace.clone(), key.clone()), record);
        merged += 1;
    }
    ctx.add_result(json!({
        "merged": merged,
        "namespace": namespace,
    }));
}

/// Ask the hook bus whether a setting value is acceptable. Returns an
/// error describing the refusal when a handler marked it invalid.
pub(crate) fn validate_via_hooks(
    ctx: &AppContext,
    namespace: &str,
    key: &str,
    value_type: &str,
    value: &Value,
) -> UnifiedResult<()> {
    let hook = ctx.hooks.emit(
        events::CONFIG_VALIDATE,
        json!({
            "namespace": namespace,
            "key": key,
            "type": value_type,
            "value": value,
            "valid": true,
        }),
    );
    let mut message: Option<String> = None;
    if hook.get("valid").and_then(|value| value.as_bool()) == Some(false) {
        message = hook
            .get("message")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());
    }
    if message.is_none() {
        for result in &hook.results {
            if result.get("valid").and_then(|value| value.as_bool()) == Some(false) {
                message = result
                    .get("message")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
                    .or_else(|| Some("Setting value rejected by validation".to_string()));
                break;
            }
        }
    }
    match message {
        Some(message) => Err(UnifiedError::unprocessable(message)),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks.
//
// Every route in this module requires an authenticated caller: the
// identity comes from the `X-User-Id` and `X-Role` headers and the
// capability is checked through the `role.check` hook, failing closed
// when no handler answers (the same guard pattern as the cache,
// webhooks, and routes modules). Reads need `settings.view`, writes
// (put/delete) need `settings.manage`.
//
// No namespace is public, so there is no unauthenticated carve-out:
// the store holds `vcs.*` / `auth.*` / `cloudflare` provider flags
// plus any namespace another module writes, and other modules mirror
// values through `settings.changed` -- an anonymous reader could
// enumerate provider configuration and an anonymous writer could
// overwrite or delete any namespace/key, including values other
// modules trust. Gating all four routes is the fix; a genuinely
// public namespace would need an explicit allowlist here first.
// ---------------------------------------------------------------------------

/// Capability checked for the `GET` routes.
pub(crate) const PERM_SETTINGS_VIEW: &str = "settings.view";
/// Capability checked for the `PUT` / `DELETE` routes.
pub(crate) const PERM_SETTINGS_MANAGE: &str = "settings.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks).
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

/// Authorize one request: caller identity plus one capability.
/// Returns the caller on success; 401 when an identity header is
/// missing, 403 when the role does not hold the permission.
pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// The `settings.changed` payload for a deleted record. Unlike the
/// put path, a bare `{ namespace, key, deleted: true }` tells
/// listeners nothing about what was removed, so the deleted
/// record's type and value travel with the event; listeners use
/// them to drop (or revert) their mirror of exactly that value.
pub(crate) fn deleted_setting_event(record: &SettingRecord) -> Value {
    json!({
        "namespace": record.namespace.clone(),
        "key": record.key.clone(),
        "type": record.value_type.clone(),
        "value": record.value.clone(),
        "deleted": true,
    })
}

pub(crate) async fn list_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    store: web::Data<SettingsStore>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SETTINGS_VIEW) {
        return error_response(&error, debug);
    }
    let sort = query.allowed_sort(&["namespace", "key", "updated_at"], "namespace");
    let pagination = query.normalized();
    let descending = pagination.order == "desc";
    let namespace_filter = pagination.filters.get("namespace").cloned();
    let search = pagination.search.clone().map(|s| s.to_lowercase());

    let mut records: Vec<SettingRecord> = store
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            if let Some(namespace) = &namespace_filter {
                if record.namespace != *namespace {
                    return false;
                }
            }
            if let Some(search) = &search {
                let haystack = format!("{} {} {}", record.namespace, record.key, record.value)
                    .to_lowercase();
                if !haystack.contains(search.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    records.sort_by(|a, b| match sort.as_str() {
        "key" => a.key.cmp(&b.key),
        "updated_at" => a.updated_at.cmp(&b.updated_at),
        _ => a
            .namespace
            .cmp(&b.namespace)
            .then_with(|| a.key.cmp(&b.key)),
    });
    if descending {
        records.reverse();
    }
    let total = records.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(records.len());
    let items = if start < records.len() {
        records[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

pub(crate) async fn get_setting(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    store: web::Data<SettingsStore>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SETTINGS_VIEW) {
        return error_response(&error, debug);
    }
    let (namespace, key) = path.into_inner();
    match store.get(&(namespace.clone(), key.clone())) {
        Some(record) => success_response(record.value().clone(), 200),
        None => error_response(
            &UnifiedError::not_found(format!("Setting \"{namespace}/{key}\" not found")),
            debug,
        ),
    }
}

pub(crate) async fn put_setting(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    store: web::Data<SettingsStore>,
    path: web::Path<(String, String)>,
    body: web::Json<PutSettingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SETTINGS_MANAGE) {
        return error_response(&error, debug);
    }
    let (namespace, key) = path.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<SettingRecord> = (|| {
        if !valid_name(&namespace) || !valid_name(&key) {
            return Err(UnifiedError::bad_request(
                "Namespace and key must contain only letters, digits, '-', '_' or '.'",
            ));
        }
        let value_type = match &input.value_type {
            Some(value_type) => {
                if !matches!(value_type.as_str(), "string" | "number" | "bool" | "json") {
                    return Err(UnifiedError::bad_request(
                        "Setting type must be \"string\", \"number\", \"bool\" or \"json\"",
                    ));
                }
                if !type_matches(value_type, &input.value) {
                    return Err(UnifiedError::bad_request(format!(
                        "Value does not match the declared type \"{value_type}\""
                    )));
                }
                value_type.clone()
            }
            None => infer_type(&input.value).to_string(),
        };
        validate_via_hooks(&ctx, &namespace, &key, &value_type, &input.value)?;
        let record = SettingRecord {
            namespace: namespace.clone(),
            key: key.clone(),
            value_type,
            value: input.value,
            updated_at: Utc::now(),
        };
        store.insert((namespace, key), record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => {
            // Batch 18: tell owning modules the value changed so
            // their effective-value mirrors stay in sync (the
            // payments module listens for `cards.*` /
            // `reference_id.*` here). Emitted only after the write
            // lands; a refused write emits nothing.
            ctx.hooks.emit(
                "settings.changed",
                json!({
                    "namespace": record.namespace.clone(),
                    "key": record.key.clone(),
                    "type": record.value_type.clone(),
                    "value": record.value.clone(),
                    "deleted": false,
                }),
            );
            success_response(record, 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn delete_setting(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    store: web::Data<SettingsStore>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SETTINGS_MANAGE) {
        return error_response(&error, debug);
    }
    let (namespace, key) = path.into_inner();
    match store.remove(&(namespace.clone(), key.clone())) {
        Some((_, removed)) => {
            // Batch 18: see `put_setting` -- a delete reverts the
            // key to its compiled-in default in listening modules.
            // The removed record's type and value travel with the
            // event so listeners know exactly what was removed.
            ctx.hooks.emit("settings.changed", deleted_setting_event(&removed));
            success_response(
                json!({ "deleted": true, "namespace": namespace, "key": key }),
                200,
            )
        }
        None => error_response(
            &UnifiedError::not_found(format!("Setting \"{namespace}/{key}\" not found")),
            debug,
        ),
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
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn allow_all_role_checks(ctx: &AppContext) {
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.set("allowed", Value::Bool(true));
            }),
        );
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&test_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
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
        // preset `"allowed": false` must stand and both reads and
        // writes are refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_SETTINGS_VIEW));
        assert!(!check_capability(&ctx, "admin", PERM_SETTINGS_MANAGE));
        assert!(authorize(&ctx, &test_request(), PERM_SETTINGS_VIEW).is_err());
        assert!(authorize(&ctx, &test_request(), PERM_SETTINGS_MANAGE).is_err());
    }

    #[test]
    fn authorize_rejects_a_request_without_a_verified_caller() {
        let ctx = test_context();
        allow_all_role_checks(&ctx);
        let req = TestRequest::default().to_http_request();
        assert!(authorize(&ctx, &req, PERM_SETTINGS_VIEW).is_err());
    }

    #[test]
    fn authorize_accepts_when_role_check_allows() {
        let ctx = test_context();
        allow_all_role_checks(&ctx);
        let found = authorize(&ctx, &test_request(), PERM_SETTINGS_VIEW).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(authorize(&ctx, &test_request(), PERM_SETTINGS_MANAGE).is_ok());
    }

    #[test]
    fn is_mergeable_namespace_cases() {
        assert!(is_mergeable_namespace("vcs.github"));
        assert!(is_mergeable_namespace("vcs.gitlab"));
        assert!(is_mergeable_namespace("auth.google"));
        assert!(is_mergeable_namespace("auth.github"));
        assert!(is_mergeable_namespace("cloudflare"));
        assert!(!is_mergeable_namespace("cloudflare.status"));
        assert!(!is_mergeable_namespace("vcs"));
        assert!(!is_mergeable_namespace("auth"));
        assert!(!is_mergeable_namespace("other"));
        assert!(!is_mergeable_namespace(""));
    }

    #[test]
    fn merge_accepts_auth_namespace() {
        // The auth manager emits `auth.{provider-id}` (see
        // `auth_manager/handlers_providers.rs`); it must land in
        // the store instead of being refused with merged: 0.
        let store: SettingsStore = Arc::new(DashMap::new());
        let mut hook_ctx = HookContext::new(
            "settings.merge",
            json!({
                "namespace": "auth.google",
                "settings": { "client_id": "id-1", "enabled": true },
            }),
        );
        on_settings_merge(store.clone(), &mut hook_ctx);
        assert_eq!(hook_ctx.results.len(), 1);
        assert_eq!(hook_ctx.results[0].get("merged").and_then(|v| v.as_u64()), Some(2));
        let record = store
            .get(&("auth.google".to_string(), "client_id".to_string()))
            .expect("merged auth record must be stored");
        assert_eq!(record.value, json!("id-1"));
    }

    #[test]
    fn merge_still_accepts_vcs_and_cloudflare_namespaces() {
        let store: SettingsStore = Arc::new(DashMap::new());
        let mut vcs_ctx = HookContext::new(
            "settings.merge",
            json!({
                "namespace": "vcs.github",
                "settings": { "token_configured": true },
            }),
        );
        on_settings_merge(store.clone(), &mut vcs_ctx);
        assert_eq!(vcs_ctx.results[0].get("merged").and_then(|v| v.as_u64()), Some(1));
        let mut cf_ctx = HookContext::new(
            "settings.merge",
            json!({
                "namespace": "cloudflare",
                "settings": { "api_token_configured": false },
            }),
        );
        on_settings_merge(store.clone(), &mut cf_ctx);
        assert_eq!(cf_ctx.results[0].get("merged").and_then(|v| v.as_u64()), Some(1));
        assert!(store
            .get(&("cloudflare".to_string(), "api_token_configured".to_string()))
            .is_some());
    }

    #[test]
    fn merge_refuses_an_unknown_namespace() {
        let store: SettingsStore = Arc::new(DashMap::new());
        let mut hook_ctx = HookContext::new(
            "settings.merge",
            json!({
                "namespace": "payments",
                "settings": { "key": "value" },
            }),
        );
        on_settings_merge(store.clone(), &mut hook_ctx);
        assert_eq!(hook_ctx.results.len(), 1);
        assert_eq!(hook_ctx.results[0].get("merged").and_then(|v| v.as_u64()), Some(0));
        assert!(hook_ctx.results[0]
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|reason| reason.contains("auth.*"))
            .unwrap_or(false));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn merge_refuses_non_object_settings() {
        let store: SettingsStore = Arc::new(DashMap::new());
        let mut hook_ctx = HookContext::new(
            "settings.merge",
            json!({ "namespace": "vcs.github", "settings": "not-an-object" }),
        );
        on_settings_merge(store.clone(), &mut hook_ctx);
        assert_eq!(hook_ctx.results[0].get("merged").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn merge_skips_invalid_keys_but_keeps_valid_ones() {
        let store: SettingsStore = Arc::new(DashMap::new());
        let mut hook_ctx = HookContext::new(
            "settings.merge",
            json!({
                "namespace": "vcs.github",
                "settings": { "good": 1, "bad key": 2 },
            }),
        );
        on_settings_merge(store.clone(), &mut hook_ctx);
        assert_eq!(hook_ctx.results[0].get("merged").and_then(|v| v.as_u64()), Some(1));
        assert!(store
            .get(&("vcs.github".to_string(), "good".to_string()))
            .is_some());
    }

    #[test]
    fn deleted_setting_event_carries_type_and_value() {
        let record = SettingRecord {
            namespace: "cards".to_string(),
            key: "brand".to_string(),
            value_type: "string".to_string(),
            value: json!("visa"),
            updated_at: Utc::now(),
        };
        let event = deleted_setting_event(&record);
        assert_eq!(event.get("namespace").and_then(|v| v.as_str()), Some("cards"));
        assert_eq!(event.get("key").and_then(|v| v.as_str()), Some("brand"));
        assert_eq!(event.get("type").and_then(|v| v.as_str()), Some("string"));
        assert_eq!(event.get("value"), Some(&json!("visa")));
        assert_eq!(event.get("deleted").and_then(|v| v.as_bool()), Some(true));
    }
}
