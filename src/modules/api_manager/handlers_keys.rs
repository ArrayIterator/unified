// Api Manager - API key and access-rule handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hooks::HookContext;
use unified_core::json as api_json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, web, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::validation::{
    PERM_API_MANAGER_MANAGE, PERM_API_MANAGER_READ, require_admin,
};

use super::{ApiAccessAuditEntry, ApiAccessRule, ApiManagerState, CreateAccessRuleInput,
    CreateKeyInput, UpdateAccessRuleInput, UpdateKeyAccessInput, validate_method, validate_path,
    validate_version};

// ---------------------------------------------------------------------------
// API key handlers.
//
// These endpoints are a thin adapter over the API Keys module
// (`crate::modules::apikeys`), the single source of truth for key
// storage, hashing, and verification. No key material is stored
// here; the handlers only translate this module's request and
// response shapes to and from the shared key service. The
// `api-manager` endpoints keep their historic field names (`key`
// for the full value shown once, `revoked` as a boolean) and gain
// the shared store's `status`, `rate_tier`,
// `requests_per_minute`, and `usage_count` fields.

pub(crate) async fn create_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreateKeyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = input.into_inner();
    // Validation (name length, scope shape, expiry range) lives in
    // the shared service; scopes are validated against the
    // registered module ids, and an omitted scope list is rejected
    // with 400 there (explicit scopes are required; no omnipotent
    // `["*"]` default).
    match crate::modules::apikeys::create_api_key(
        crate::modules::apikeys::CreateApiKeyParams {
            name: input.name.trim().to_string(),
            owner_id: None,
            scopes: input.scopes,
            rate_tier: None,
            requests_per_minute: None,
            expires_in_days: input.expires_in_days,
        },
        &ctx.modules.ids(),
    ) {
        Ok(created) => {
            let key = created.key;
            // The full key is returned exactly once, at creation time.
            api_json::success_response(
                json!({
                    "id": key.id,
                    "name": key.name,
                    "key": created.full_key,
                    "prefix": key.prefix,
                    "scopes": key.scopes,
                    "status": key.status,
                    "rate_tier": key.rate_tier,
                    "requests_per_minute": key.requests_per_minute,
                    "created_at": key.created_at,
                    "expires_at": key.expires_at,
                    "revoked": key.status == "revoked",
                }),
                201,
            )
        }
        Err(error) => api_json::error_response(&error, debug),
    }
}

pub(crate) async fn list_keys(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let search = params.search.clone();
    let revoked_filter = params.filters.get("revoked").cloned();
    let mut items: Vec<crate::modules::apikeys::ApiKeyView> =
        crate::modules::apikeys::all_key_views()
            .into_iter()
            .filter(|key| {
                if let Some(revoked) = &revoked_filter {
                    let wanted = revoked == "true";
                    if (key.status == "revoked") != wanted {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let needle = needle.to_lowercase();
                    return key.name.to_lowercase().contains(&needle)
                        || key.prefix.to_lowercase().contains(&needle);
                }
                true
            })
            .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let items: Vec<Value> = items
        .into_iter()
        .map(|key| {
            json!({
                "id": key.id,
                "name": key.name,
                "prefix": key.prefix,
                "scopes": key.scopes,
                "status": key.status,
                "rate_tier": key.rate_tier,
                "requests_per_minute": key.requests_per_minute,
                "created_at": key.created_at,
                "expires_at": key.expires_at,
                "last_used_at": key.last_used_at,
                "usage_count": key.usage_count,
                "revoked": key.status == "revoked",
            })
        })
        .collect();
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<Value> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

pub(crate) async fn revoke_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = id.into_inner();
    match crate::modules::apikeys::revoke_api_key(id) {
        Ok(view) => api_json::success_response(
            json!({ "id": view.id, "revoked": true }),
            200,
        ),
        Err(error) => api_json::error_response(&error, debug),
    }
}

/// Audit one API-key lifecycle event emitted by the API Keys
/// module. The payload carries no actor, so the key owner (when
/// known) is recorded as the actor and `system` otherwise. The
/// entry is persisted through `record_access_audit` like every
/// other access-control change.
pub(crate) fn on_apikey_created(state: &ApiManagerState, ctx: &mut HookContext) {
    let key_id = ctx
        .get("key_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let actor = ctx
        .get("owner_id")
        .and_then(|value| value.as_str())
        .filter(|text| !text.is_empty())
        .unwrap_or("system")
        .to_string();
    state.record_access_audit(
        &actor,
        "key_created",
        &key_id,
        json!({
            "name": ctx.get("name"),
            "scopes": ctx.get("scopes"),
            "rate_tier": ctx.get("rate_tier"),
            "expires_at": ctx.get("expires_at"),
        }),
    );
    ctx.add_result(json!({"audited": true, "action": "key_created", "target": key_id}));
}

/// Audit an `apikey.revoked` event (manual revocation, or a rotation
/// grace period expiring) as a `key_revoked` audit entry.
pub(crate) fn on_apikey_revoked(state: &ApiManagerState, ctx: &mut HookContext) {
    let key_id = ctx
        .get("key_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    state.record_access_audit(
        "system",
        "key_revoked",
        &key_id,
        json!({ "reason": ctx.get("reason") }),
    );
    ctx.add_result(json!({"audited": true, "action": "key_revoked", "target": key_id}));
}

/// Audit an `apikey.rotated` event: rotation creates a replacement
/// key, so the new key is audited as `key_created` with the old key
/// recorded in the detail. (The rotation path emits only
/// `apikey.rotated`, not `apikey.created`, so there is no double
/// audit for the same key.)
pub(crate) fn on_apikey_rotated(state: &ApiManagerState, ctx: &mut HookContext) {
    let new_key_id = ctx
        .get("new_key_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    state.record_access_audit(
        "system",
        "key_created",
        &new_key_id,
        json!({
            "rotated_from": ctx.get("old_key_id"),
            "grace_until": ctx.get("grace_until"),
        }),
    );
    ctx.add_result(json!({"audited": true, "action": "key_created", "target": new_key_id}));
}

// ---------------------------------------------------------------------------
// Access-control handlers.
//
// Rules and the audit trail live on `ApiManagerState` (in memory)
// and are persisted to the migration 000004 tables
// (`api_access_rules`, `api_access_audit`) whenever a database pool
// is attached; see `load_access_from_db` / `persist_access_rule` /
// `persist_access_audit_entry`. Per-key scopes and rate limits live
// in the API Keys module; these handlers read them through its
// views and write them through its `update_api_key_access` service
// function, so this module never touches key records directly. Key
// material is never returned: only the key id, name, scopes, tier,
// and limit.
// ---------------------------------------------------------------------------

pub(crate) async fn list_access_rules(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut rules: Vec<ApiAccessRule> = state
        .access_rules
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    rules.sort_by(|a, b| {
        a.method
            .cmp(&b.method)
            .then(a.path.cmp(&b.path))
            .then(a.version.cmp(&b.version))
    });
    let pagination = params.normalized();
    let total = rules.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiAccessRule> = rules
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

pub(crate) async fn create_access_rule(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreateAccessRuleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The audit actor is the authenticated caller, never the
    // client-supplied `actor` body field: a caller must not be able
    // to forge another identity into the audit trail.
    let caller = match require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let input = input.into_inner();
    let result: Result<ApiAccessRule, UnifiedError> = (|| {
        let method = validate_method(&input.method)?;
        let path = validate_path(&input.path)?;
        let version = match &input.version {
            Some(version) => {
                let version = validate_version(version)?;
                if !state.versions.contains_key(&version) {
                    return Err(UnifiedError::bad_request(format!(
                        "unknown API version \"{version}\""
                    )));
                }
                Some(version)
            }
            None => None,
        };
        let key = ApiManagerState::access_rule_key(&method, &path, version.as_deref());
        if state.access_rules.contains_key(&key) {
            return Err(UnifiedError::conflict(format!(
                "an access rule already exists for {method} {path}{}",
                version
                    .as_ref()
                    .map(|version| format!("@{version}"))
                    .unwrap_or_default()
            )));
        }
        let actor = caller.actor.clone();
        let now = Utc::now();
        let rule = ApiAccessRule {
            id: Uuid::now_v7(),
            method,
            path,
            version,
            allowed: input.allowed,
            public_access: input.public_access,
            actor: actor.clone(),
            reason: input.reason.clone(),
            created_at: now,
            updated_at: now,
        };
        state.access_rules.insert(key, rule.clone());
        state.persist_access_rule(&rule);
        state.record_access_audit(
            &actor,
            "rule_created",
            &format!("{} {}@{}", rule.method, rule.path, rule.version.as_deref().unwrap_or("*")),
            json!({
                "rule_id": rule.id,
                "allowed": rule.allowed,
                "public_access": rule.public_access,
                "reason": rule.reason,
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => api_json::success_response(json!({ "rule": rule }), 201),
        Err(error) => api_json::error_response(&error, debug),
    }
}

pub(crate) async fn update_access_rule(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<UpdateAccessRuleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The audit actor is the authenticated caller, never the
    // client-supplied `actor` body field (see `create_access_rule`).
    let caller = match require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let id = id.into_inner();
    let input = input.into_inner();
    let result: Result<ApiAccessRule, UnifiedError> = (|| {
        let key = state
            .access_rules
            .iter()
            .find(|entry| entry.value().id == id)
            .map(|entry| entry.key().clone())
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        let mut rule = state
            .access_rules
            .get(&key)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        let mut changed: Vec<&str> = Vec::new();
        if let Some(allowed) = input.allowed {
            if rule.allowed != allowed {
                rule.allowed = allowed;
                changed.push("allowed");
            }
        }
        if let Some(public_access) = input.public_access {
            if rule.public_access != public_access {
                rule.public_access = public_access;
                changed.push("public_access");
            }
        }
        // The stored actor is always the authenticated caller, so a
        // request body cannot forge the audit identity; the
        // client-supplied `actor` field is ignored.
        rule.actor = caller.actor.clone();
        if let Some(reason) = &input.reason {
            rule.reason = Some(reason.clone());
        }
        rule.updated_at = Utc::now();
        let actor = rule.actor.clone();
        state.access_rules.insert(key, rule.clone());
        state.persist_access_rule(&rule);
        state.record_access_audit(
            &actor,
            "rule_updated",
            &format!("{} {}@{}", rule.method, rule.path, rule.version.as_deref().unwrap_or("*")),
            json!({
                "rule_id": rule.id,
                "changed": changed,
                "allowed": rule.allowed,
                "public_access": rule.public_access,
                "reason": rule.reason,
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => api_json::success_response(json!({ "rule": rule }), 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_access_rule(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: Result<ApiAccessRule, UnifiedError> = (|| {
        let key = state
            .access_rules
            .iter()
            .find(|entry| entry.value().id == id)
            .map(|entry| entry.key().clone())
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        let (_, rule) = state
            .access_rules
            .remove(&key)
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        state.delete_access_rule_from_db(&rule);
        state.record_access_audit(
            &rule.actor,
            "rule_deleted",
            &format!("{} {}@{}", rule.method, rule.path, rule.version.as_deref().unwrap_or("*")),
            json!({ "rule_id": rule.id }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => api_json::success_response(json!({ "deleted": true, "id": rule.id }), 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

pub(crate) async fn list_access_audit(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut entries: Vec<ApiAccessAuditEntry> = state
        .access_audit
        .read()
        .map(|audit| audit.clone())
        .unwrap_or_default();
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let pagination = params.normalized();
    let total = entries.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiAccessAuditEntry> = entries
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

pub(crate) async fn get_key_access(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_admin(&ctx, &req, PERM_API_MANAGER_READ) {
        return api_json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: Result<Value, UnifiedError> = (|| {
        let view = crate::modules::apikeys::all_key_views()
            .into_iter()
            .find(|key| key.id == id)
            .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
        Ok(json!({
            "id": view.id,
            "name": view.name,
            "scopes": view.scopes,
            "rate_tier": view.rate_tier,
            "requests_per_minute": view.requests_per_minute,
            "status": view.status,
            "usage_count": view.usage_count,
        }))
    })();
    match result {
        Ok(value) => api_json::success_response(value, 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

pub(crate) async fn update_key_access(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApiManagerState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<UpdateKeyAccessInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The audit actor is the authenticated caller, never the
    // client-supplied `actor` body field (see `create_access_rule`).
    let caller = match require_admin(&ctx, &req, PERM_API_MANAGER_MANAGE) {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let id = id.into_inner();
    let input = input.into_inner();
    let actor = caller.actor.clone();
    let scopes_changed = input.scopes.is_some();
    let limit_changed = input.rate_tier.is_some() || input.requests_per_minute.is_some();
    let result = crate::modules::apikeys::update_api_key_access(
        id,
        input.scopes.clone(),
        input.rate_tier.clone(),
        input.requests_per_minute,
        &ctx.modules.ids(),
    );
    match result {
        Ok(view) => {
            if scopes_changed {
                state.record_access_audit(
                    &actor,
                    "key_scopes_updated",
                    &id.to_string(),
                    json!({ "scopes": view.scopes }),
                );
            }
            if limit_changed {
                state.record_access_audit(
                    &actor,
                    "key_rate_limit_updated",
                    &id.to_string(),
                    json!({
                        "rate_tier": view.rate_tier,
                        "requests_per_minute": view.requests_per_minute,
                    }),
                );
            }
            api_json::success_response(
                json!({
                    "id": view.id,
                    "name": view.name,
                    "scopes": view.scopes,
                    "rate_tier": view.rate_tier,
                    "requests_per_minute": view.requests_per_minute,
                    "status": view.status,
                    "usage_count": view.usage_count,
                }),
                200,
            )
        }
        Err(error) => api_json::error_response(&error, debug),
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
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(AppConfig::default(), hooks, Arc::new(ModuleRegistry::new()));
        web::Data::new(Arc::new(ctx))
    }

    fn state_data() -> web::Data<ApiManagerState> {
        let state = ApiManagerState::default();
        state.ensure_seeded_versions();
        web::Data::new(state)
    }

    fn authed_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn create_key_input() -> web::Json<CreateKeyInput> {
        web::Json(CreateKeyInput {
            name: "handler test key".to_string(),
            scopes: Some(vec!["*".to_string()]),
            expires_in_days: None,
        })
    }

    fn create_rule_input() -> web::Json<CreateAccessRuleInput> {
        web::Json(CreateAccessRuleInput {
            method: "GET".to_string(),
            path: "/things".to_string(),
            version: Some("v1".to_string()),
            allowed: true,
            public_access: true,
            // A forged actor: the stored rule must record the
            // authenticated caller instead.
            actor: Some("mallory".to_string()),
            reason: None,
        })
    }

    #[tokio::test]
    async fn create_key_rejects_anonymous_callers() {
        let resp = create_key(
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            anonymous_request(),
            create_key_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_key_rejects_callers_without_manage() {
        let resp = create_key(
            test_ctx(&[PERM_API_MANAGER_READ]),
            authed_request(),
            create_key_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_keys_rejects_anonymous_callers() {
        let resp = list_keys(
            test_ctx(&[PERM_API_MANAGER_READ]),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn revoke_key_rejects_anonymous_callers() {
        let resp = revoke_key(
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            anonymous_request(),
            web::Path::from(Uuid::now_v7()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_access_rule_rejects_anonymous_callers() {
        let resp = create_access_rule(
            state_data(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            anonymous_request(),
            create_rule_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_access_rule_records_authenticated_caller_as_actor() {
        let state = state_data();
        let resp = create_access_rule(
            state.clone(),
            test_ctx(&[PERM_API_MANAGER_MANAGE]),
            authed_request(),
            create_rule_input(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let rule = state
            .access_rules
            .iter()
            .next()
            .expect("rule was stored")
            .value()
            .clone();
        assert_eq!(rule.actor, test_user_uuid("tester").to_string());
    }
}
