//! Cloudflare connector module. - Helpers.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::module::*;
use super::requests::*;
use super::state::*;
use super::status_handlers::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use serde::Serialize;
use serde_json::{json, Value};

pub(super) fn mask_token(token: &str) -> String {
    let tail: String = token.chars().rev().take(4).collect::<String>().chars().rev().collect();
    format!("****{tail}")
}

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the VCS and elearning
/// modules. Every handler in this module resolves its caller
/// through this helper first; there is no anonymous access.
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
/// (same fail-closed shape as the VCS and elearning modules).
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

pub(super) fn require_permission(
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

/// Caller identity plus capability check in one call: anonymous
/// callers are refused with 401, and roles without the permission
/// are refused with 403. Every handler in this module starts with
/// this guard - read-only routes pass `PERM_CLOUDFLARE_VIEW`,
/// state-changing routes pass `PERM_CLOUDFLARE_MANAGE`.
pub(super) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

pub(super) fn paginate_local<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> Value {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination).to_json()
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

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", Value::Bool(true));
            }),
        );
        ctx
    }

    fn identified_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&identified_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert_eq!(caller(&req).unwrap_err().code, 401);
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
    fn authorize_rejects_anonymous_callers() {
        let ctx = test_context();
        let req = TestRequest::default().to_http_request();
        assert_eq!(
            authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)
                .unwrap_err()
                .code,
            401
        );
    }

    #[test]
    fn authorize_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert_eq!(
            authorize(&ctx, &identified_request(), PERM_CLOUDFLARE_VIEW)
                .unwrap_err()
                .code,
            403
        );
        assert!(!check_capability(&ctx, "admin", PERM_CLOUDFLARE_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_CLOUDFLARE_MANAGE).is_err());
    }

    #[test]
    fn authorize_accepts_when_role_check_allows() {
        let ctx = allowing_context();
        let found = authorize(&ctx, &identified_request(), PERM_CLOUDFLARE_MANAGE).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(check_capability(&ctx, "admin", PERM_CLOUDFLARE_VIEW));
        assert!(require_permission(&ctx, "admin", PERM_CLOUDFLARE_VIEW).is_ok());
    }

    #[test]
    fn mask_token_keeps_last_four_chars() {
        let masked = mask_token("abcdefgh");
        assert!(masked.ends_with("efgh"));
        assert_eq!(masked.chars().count(), 8);
        let short = mask_token("ab");
        assert!(short.ends_with("ab"));
        assert_eq!(short.chars().count(), 6);
        let empty = mask_token("");
        assert_eq!(empty.chars().count(), 4);
    }

    #[test]
    fn paginate_local_slices_and_reports_totals() {
        let page = paginate_local(vec![1, 2, 3, 4, 5], &PaginationParams::default());
        assert_eq!(page["items"].as_array().unwrap().len(), 5);
        assert_eq!(page["pagination"]["total"], 5);
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let second = paginate_local(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(second["items"].as_array().unwrap().len(), 2);
        assert_eq!(second["items"][0], 3);
        assert_eq!(second["pagination"]["total_pages"], 3);
    }
}
