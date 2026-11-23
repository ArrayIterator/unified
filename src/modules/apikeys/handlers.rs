//! apikeys module - HTTP handlers for API key management.
//!
//! Every management endpoint is admin-gated: the caller is
//! identified from the `X-User-Id` / `X-Role` headers and must hold
//! `apikeys.view` (reads) or `apikeys.manage` (mutations) through
//! the `role.check` hook, failing closed when the hook does not
//! answer. `POST /verify` is gated the same way (`apikeys.view`)
//! and returns only `valid` plus the rate tier -- never the key id,
//! owner id, or scopes -- and records a successful verification as
//! usage.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::apikey_events;
use super::constants::DEFAULT_ROTATION_GRACE_SECS;
use super::constants::MAX_EXPIRY_DAYS;
use super::constants::MAX_ROTATION_GRACE_SECS;
use super::constants::PERM_APIKEYS_MANAGE;
use super::constants::PERM_APIKEYS_VIEW;
use super::constants::SCOPE_ACTIONS;
use super::service::CreateApiKeyParams;
use super::service::all_key_views;
use super::service::authenticate_key;
use super::service::create_api_key;
use super::service::generate_key_value;
use super::service::hash_key_value_peppered;
use super::service::note_used;
use super::service::revoke_api_key;
use super::service::tier_limit;
use super::service::validate_scopes;
use super::store::emit;
use super::store::hash_index;
use super::store::keys_store;
use super::types::ApiKeyCreatedView;
use super::types::ApiKeyRecord;
use super::types::ApiKeyStatus;
use super::types::ApiKeyView;

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateKeyRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) owner_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) scopes: Option<Vec<String>>,
    /// Rate tier: `standard` (60/min), `elevated` (300/min), `high` (1000/min).
    #[serde(default)]
    pub(crate) rate_tier: Option<String>,
    /// Explicit per-minute limit; overrides the tier when present.
    #[serde(default)]
    pub(crate) requests_per_minute: Option<u32>,
    /// Lifetime in days from creation (1..=3650). Omitted: never expires.
    #[serde(default)]
    pub(crate) expires_in_days: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateKeyRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) scopes: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) rate_tier: Option<String>,
    #[serde(default)]
    pub(crate) requests_per_minute: Option<u32>,
    /// New expiry in days from now (1..=3650). Explicit null clears it.
    #[serde(default)]
    pub(crate) expires_in_days: Option<Option<u32>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RotateKeyRequest {
    /// Grace period in seconds for the old key (1..=2592000, default 86400).
    #[serde(default)]
    pub(crate) grace_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VerifyKeyRequest {
    pub(crate) key: String,
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

pub(crate) fn find_key(id: Uuid) -> UnifiedResult<ApiKeyRecord> {
    keys_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("API key not found"))
}

// ---------------------------------------------------------------------------
// Caller authentication (calculator/bi/supervision pattern).
// ---------------------------------------------------------------------------

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
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

/// Resolve the caller from the request headers and require
/// `permission` for them. Use `PERM_APIKEYS_VIEW` for reads and
/// `PERM_APIKEYS_MANAGE` for mutations.
pub(crate) fn require_admin(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn create_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_admin(&ctx, &req, PERM_APIKEYS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result = create_api_key(
        CreateApiKeyParams {
            name: body.name.clone(),
            owner_id: body.owner_id,
            scopes: body.scopes.clone(),
            rate_tier: body.rate_tier.clone(),
            requests_per_minute: body.requests_per_minute,
            expires_in_days: body.expires_in_days,
        },
        &ctx.modules.ids(),
    );
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_keys(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_admin(&ctx, &req, PERM_APIKEYS_VIEW)?;
        let mut items: Vec<ApiKeyView> = all_key_views();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|key| {
                key.name.to_lowercase().contains(&needle)
                    || key.prefix.to_lowercase().contains(&needle)
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|key| key.status == *status);
        }
        if let Some(owner) = params.filters.get("owner_id") {
            items.retain(|key| {
                key.owner_id
                    .map(|id| id.to_string() == *owner)
                    .unwrap_or(false)
            });
        }
        if let Some(scope) = params.filters.get("scope") {
            items.retain(|key| key.scopes.iter().any(|item| item == scope));
        }
        let sort = params.allowed_sort(
            &["created_at", "name", "usage_count", "last_used_at"],
            "created_at",
        );
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "usage_count" => items.sort_by(|a, b| a.usage_count.cmp(&b.usage_count)),
            "last_used_at" => items.sort_by(|a, b| a.last_used_at.cmp(&b.last_used_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApiKeyView> = (|| {
        require_admin(&ctx, &req, PERM_APIKEYS_VIEW)?;
        find_key(id.into_inner()).map(|record| record.view())
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_scopes(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_admin(&ctx, &req, PERM_APIKEYS_VIEW)?;
        let modules = ctx.modules.summary();
        let scopes: Vec<Value> = modules
            .iter()
            .map(|module| {
                serde_json::json!({
                    "module": module.id,
                    "name": module.name,
                    "actions": SCOPE_ACTIONS,
                    "scopes": SCOPE_ACTIONS.iter().map(|action| format!("{}:{action}", module.id)).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "modules": scopes,
            "wildcard": "*",
            "actions": SCOPE_ACTIONS,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ApiKeyView> = (|| {
        require_admin(&ctx, &req, PERM_APIKEYS_MANAGE)?;
        if let Some(name) = &body.name {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 120 {
                return Err(UnifiedError::bad_request(
                    "name must be non-empty and at most 120 characters",
                ));
            }
        }
        if let Some(scopes) = &body.scopes {
            validate_scopes(scopes, &ctx.modules.ids())?;
        }
        if let Some(tier) = &body.rate_tier {
            if tier_limit(tier).is_none() {
                return Err(UnifiedError::bad_request(
                    "rate_tier must be one of: standard, elevated, high",
                ));
            }
        }
        if let Some(limit) = body.requests_per_minute {
            if limit == 0 || limit > 100_000 {
                return Err(UnifiedError::bad_request(
                    "requests_per_minute must be between 1 and 100000",
                ));
            }
        }
        let expires_at = match body.expires_in_days {
            Some(Some(days)) => {
                if days == 0 || days > MAX_EXPIRY_DAYS {
                    return Err(UnifiedError::bad_request(format!(
                        "expires_in_days must be between 1 and {MAX_EXPIRY_DAYS}"
                    )));
                }
                Some(Some(Utc::now() + chrono::Duration::days(i64::from(days))))
            }
            Some(None) => Some(None),
            None => None,
        };
        let mut record = keys_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
        // Revoked, expired, and rotating keys cannot be updated: a
        // key mid-rotation must keep the scopes and tier it was
        // rotated with.
        if record.status == ApiKeyStatus::Revoked
            || record.status == ApiKeyStatus::Expired
            || record.status == ApiKeyStatus::Rotating
        {
            return Err(UnifiedError::conflict(format!(
                "Cannot update an API key in status \"{}\"",
                record.status.as_str()
            )));
        }
        if let Some(name) = &body.name {
            record.name = name.trim().to_string();
        }
        if let Some(scopes) = &body.scopes {
            record.scopes = scopes.clone();
        }
        if let Some(tier) = &body.rate_tier {
            record.rate_tier = tier.clone();
            if body.requests_per_minute.is_none() {
                record.requests_per_minute = tier_limit(tier).unwrap_or(60);
            }
        }
        if let Some(limit) = body.requests_per_minute {
            record.requests_per_minute = limit;
        }
        if let Some(next) = expires_at {
            record.expires_at = next;
        }
        record.updated_at = Utc::now();
        Ok(record.view())
    })();
    json::respond(result, debug)
}

pub(crate) async fn rotate_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<RotateKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ApiKeyCreatedView> = (|| {
        require_admin(&ctx, &req, PERM_APIKEYS_MANAGE)?;
        let grace_secs = body.grace_secs.unwrap_or(DEFAULT_ROTATION_GRACE_SECS);
        if grace_secs < 1 || grace_secs > MAX_ROTATION_GRACE_SECS {
            return Err(UnifiedError::bad_request(format!(
                "grace_secs must be between 1 and {MAX_ROTATION_GRACE_SECS}"
            )));
        }
        let mut old = keys_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
        if old.status == ApiKeyStatus::Revoked || old.status == ApiKeyStatus::Expired {
            return Err(UnifiedError::conflict(format!(
                "Cannot rotate an API key in status \"{}\"",
                old.status.as_str()
            )));
        }
        if old.rotated_to.is_some() {
            return Err(UnifiedError::conflict(
                "This API key was already rotated; rotate the replacement key instead",
            ));
        }
        let full_key = generate_key_value();
        let now = Utc::now();
        let grace_until = now + chrono::Duration::seconds(grace_secs);
        let new_record = ApiKeyRecord {
            id: Uuid::now_v7(),
            name: old.name.clone(),
            owner_id: old.owner_id,
            prefix: format!("{}...", &full_key[..8]),
            last4: full_key[full_key.len() - 4..].to_string(),
            key_hash: hash_key_value_peppered(&full_key),
            scopes: old.scopes.clone(),
            rate_tier: old.rate_tier.clone(),
            requests_per_minute: old.requests_per_minute,
            status: ApiKeyStatus::Active,
            expires_at: old.expires_at,
            grace_until: None,
            rotated_from: Some(old.id),
            rotated_to: None,
            created_at: now,
            updated_at: now,
            last_used_at: None,
            usage_count: 0,
        };
        let view = new_record.view();
        let new_id = new_record.id;
        old.status = ApiKeyStatus::Rotating;
        old.grace_until = Some(grace_until);
        old.rotated_to = Some(new_id);
        old.updated_at = now;
        drop(old);
        hash_index().insert(new_record.key_hash.clone(), new_record.id);
        keys_store().insert(new_record.id, new_record);
        emit(
            apikey_events::ROTATED,
            serde_json::json!({
                "old_key_id": id,
                "new_key_id": new_id,
                "grace_until": grace_until,
            }),
        );
        Ok(ApiKeyCreatedView {
            key: view,
            full_key,
        })
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn revoke_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApiKeyView> = (|| {
        require_admin(&ctx, &req, PERM_APIKEYS_MANAGE)?;
        revoke_api_key(id.into_inner())
    })();
    json::respond(result, debug)
}

pub(crate) async fn key_usage(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_admin(&ctx, &req, PERM_APIKEYS_VIEW)?;
        let record = find_key(id.into_inner())?;
        Ok(serde_json::json!({
            "key_id": record.id,
            "name": record.name,
            "prefix": record.prefix,
            "status": record.status.as_str(),
            "usage_count": record.usage_count,
            "last_used_at": record.last_used_at,
            "rate_tier": record.rate_tier,
            "requests_per_minute": record.requests_per_minute,
            "created_at": record.created_at,
            "expires_at": record.expires_at,
        }))
    })();
    json::respond(result, debug)
}

/// Verify a presented key. Admin-gated (`apikeys.view`): the
/// endpoint is not an unauthenticated oracle. A successful
/// verification returns only `valid` plus the rate tier -- never
/// the key id, owner id, scopes, or per-minute limit -- and is
/// recorded as usage via `note_used`, so verification leaves a
/// usage trace. A failed verification returns only
/// `{"valid": false}` with no reason, so the response cannot
/// distinguish an unknown key from a revoked or expired one.
pub(crate) async fn verify_key(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<VerifyKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_admin(&ctx, &req, PERM_APIKEYS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| match authenticate_key(&body.key) {
        Ok(authenticated) => {
            note_used(authenticated.id);
            Ok(serde_json::json!({
                "valid": true,
                "rate_tier": authenticated.rate_tier,
            }))
        }
        Err(_) => Ok(serde_json::json!({
            "valid": false,
        })),
    })();
    json::respond(result, debug)
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
    use actix_web::test::{TestRequest, read_body};

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

    fn authed_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn create_body(scopes: Option<Vec<String>>) -> web::Json<CreateKeyRequest> {
        web::Json(CreateKeyRequest {
            name: "handler test key".to_string(),
            owner_id: None,
            scopes,
            rate_tier: None,
            requests_per_minute: None,
            expires_in_days: None,
        })
    }

    /// Create a key through the service (explicit scopes) and return
    /// its id plus the full key value.
    fn seed_key() -> (Uuid, String) {
        let created = create_api_key(
            CreateApiKeyParams {
                name: "seeded handler key".to_string(),
                owner_id: None,
                scopes: Some(vec!["*".to_string()]),
                rate_tier: None,
                requests_per_minute: None,
                expires_in_days: None,
            },
            &[],
        )
        .unwrap();
        (created.key.id, created.full_key)
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("tester").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_requests_without_a_verified_caller() {
        assert!(caller(&anonymous_request()).is_err());
        let no_role = TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        assert!(!check_capability(&ctx, "admin", PERM_APIKEYS_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_APIKEYS_MANAGE).is_err());
    }

    #[tokio::test]
    async fn create_key_rejects_anonymous_callers() {
        let resp = create_key(
            test_ctx(&[PERM_APIKEYS_MANAGE]),
            anonymous_request(),
            create_body(Some(vec!["*".to_string()])),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_key_rejects_callers_without_manage() {
        let resp = create_key(
            test_ctx(&[PERM_APIKEYS_VIEW]),
            authed_request(),
            create_body(Some(vec!["*".to_string()])),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_key_accepts_admin_with_explicit_scopes() {
        let resp = create_key(
            test_ctx(&[PERM_APIKEYS_MANAGE]),
            authed_request(),
            create_body(Some(vec!["*".to_string()])),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_key_rejects_omitted_scopes_for_admin() {
        // No default scope: an admin omitting scopes gets 400, not
        // an omnipotent key.
        let resp = create_key(
            test_ctx(&[PERM_APIKEYS_MANAGE]),
            authed_request(),
            create_body(None),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_keys_rejects_anonymous_and_unprivileged_callers() {
        let resp = list_keys(
            test_ctx(&[PERM_APIKEYS_VIEW]),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = list_keys(
            test_ctx(&[]),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_key_rejects_anonymous_callers() {
        let (id, _key) = seed_key();
        let resp = get_key(
            test_ctx(&[PERM_APIKEYS_VIEW]),
            anonymous_request(),
            web::Path::from(id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn read_endpoints_reject_anonymous_callers() {
        let (id, _key) = seed_key();
        let resp = list_scopes(test_ctx(&[PERM_APIKEYS_VIEW]), anonymous_request()).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = key_usage(
            test_ctx(&[PERM_APIKEYS_VIEW]),
            anonymous_request(),
            web::Path::from(id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = revoke_key(
            test_ctx(&[PERM_APIKEYS_MANAGE]),
            anonymous_request(),
            web::Path::from(id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn verify_key_rejects_anonymous_callers() {
        let (_id, full_key) = seed_key();
        let resp = verify_key(
            test_ctx(&[PERM_APIKEYS_VIEW]),
            anonymous_request(),
            web::Json(VerifyKeyRequest { key: full_key }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn verify_key_returns_minimal_response_and_records_usage() {
        let (id, full_key) = seed_key();
        let resp = verify_key(
            test_ctx(&[PERM_APIKEYS_VIEW]),
            authed_request(),
            web::Json(VerifyKeyRequest { key: full_key }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
        assert!(body.contains("\"valid\":true"));
        assert!(body.contains("rate_tier"));
        // No oracle metadata: no key id, owner id, scopes, or limit.
        assert!(!body.contains("key_id"));
        assert!(!body.contains("owner_id"));
        assert!(!body.contains("scopes"));
        assert!(!body.contains("requests_per_minute"));
        // Verification leaves a usage trace.
        assert_eq!(keys_store().get(&id).unwrap().usage_count, 1);
    }

    #[tokio::test]
    async fn verify_key_invalid_key_returns_valid_false_without_reason() {
        let resp = verify_key(
            test_ctx(&[PERM_APIKEYS_VIEW]),
            authed_request(),
            web::Json(VerifyKeyRequest {
                key: "unf_doesnotexist000000000000000000000000".to_string(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
        assert!(body.contains("\"valid\":false"));
        assert!(!body.contains("reason"));
    }

    #[tokio::test]
    async fn update_key_blocks_rotating_keys() {
        let (id, _key) = seed_key();
        {
            let mut record = keys_store().get_mut(&id).unwrap();
            record.status = ApiKeyStatus::Rotating;
            record.grace_until = Some(Utc::now() + chrono::Duration::hours(1));
        }
        let resp = update_key(
            test_ctx(&[PERM_APIKEYS_MANAGE]),
            authed_request(),
            web::Path::from(id),
            web::Json(UpdateKeyRequest {
                name: Some("renamed mid-rotation".to_string()),
                scopes: None,
                rate_tier: None,
                requests_per_minute: None,
                expires_in_days: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(keys_store().get(&id).unwrap().name, "seeded handler key");
    }
}
