// Saved Views - Saved-view handlers.
//
// Every handler derives the requester identity server-side from the
// `X-User-Id` / `X-Role` headers (`caller(&req)?`) and checks the
// `saved_views.view` / `saved_views.manage` capability through the
// `role.check` hook. The caller identity is never taken from the
// request body or from a query parameter: `create_view` always
// stores the caller's own id, and `list_views` / `get_default_view`
// always scope to the caller, so passing another user's id in
// `?user_id=` discloses nothing. Single-view reads honour
// `is_visible_to` (own or shared); every mutation additionally
// requires ownership of the view.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{
    ApplyViewResponse, CreateSavedView, PERM_SAVED_VIEWS_MANAGE, PERM_SAVED_VIEWS_VIEW, SavedView,
    SavedViewsState, UpdateSavedView, caller, caller_company, get_view, is_visible_to,
    matches_search, paginate, require_caller_company, require_permission, require_tenant,
    require_view_owner, require_view_visible,
};

// Handlers.

pub(crate) async fn list_views(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<SavedView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_VIEW)?;
        let params = query.into_inner();
        // The requester is the authenticated caller, never the
        // caller-supplied `?user_id=` filter: passing a victim's id
        // must not disclose their private views.
        let requester_id = caller.actor.clone();
        let module_filter = params.filters.get("module_id").cloned();
        if let Some(module_id) = &module_filter {
            if ctx.modules.get(module_id).is_none() {
                return Err(UnifiedError::not_found(format!(
                    "Module \"{module_id}\" is not registered"
                )));
            }
        }
        let mut items: Vec<SavedView> = state
            .views
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|view| {
                matches_search(&format!("{} {}", view.name, view.module_id), &params)
                    && module_filter
                        .as_ref()
                        .is_none_or(|module_id| view.module_id == *module_id)
                    && params
                        .filters
                        .get("visibility")
                        .is_none_or(|visibility| view.visibility == *visibility)
                    && params
                        .filters
                        .get("is_default")
                        .is_none_or(|is_default| {
                            view.is_default == (*is_default == "true" || *is_default == "1")
                        })
                    && is_visible_to(view, Some(requester_id.as_str()))
            })
            .collect();
        // Defaults first, then newest first.
        items.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then(b.updated_at.cmp(&a.updated_at))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    body: web::Json<CreateSavedView>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_MANAGE)?;
        let mut input = body.into_inner();
        // The owner is always the authenticated caller: a body
        // `user_id` naming someone else is ignored, never trusted.
        input.user_id = caller.actor;
        state.create(&ctx, input)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_view_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_VIEW)?;
        let view = get_view(&state, path.as_str())?;
        require_view_visible(&view, Some(caller.actor.as_str()))?;
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSavedView>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_MANAGE)?;
        state.update(&ctx, path.as_str(), &caller.actor, body.into_inner())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_MANAGE)?;
        let view = get_view(&state, path.as_str())?;
        require_view_owner(&view, &caller.actor)?;
        state
            .views
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Saved view not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn set_default_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_MANAGE)?;
        let mut view = get_view(&state, path.as_str())?;
        require_view_owner(&view, &caller.actor)?;
        view.is_default = true;
        view.updated_at = Utc::now();
        state.views.insert(view.id.clone(), view.clone());
        state.clear_other_defaults(&view.user_id, &view.module_id, &view.id);
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn apply_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApplyViewResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_VIEW)?;
        let view = get_view(&state, path.as_str())?;
        require_view_visible(&view, Some(caller.actor.as_str()))?;
        Ok(ApplyViewResponse {
            view_id: view.id,
            user_id: view.user_id,
            module_id: view.module_id,
            name: view.name,
            filter: view.filter,
            sort: view.sort,
            order: view.order,
        })
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_default_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAVED_VIEWS_VIEW)?;
        let params = query.into_inner();
        // The owner is always the authenticated caller; a
        // `?user_id=` query value is ignored, never trusted.
        let user_id = caller.actor.clone();
        let module_id = params.filters.get("module_id").cloned().unwrap_or_default();
        if module_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Query parameter module_id is required",
            ));
        }
        if ctx.modules.get(&module_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Module \"{module_id}\" is not registered"
            )));
        }
        state
            .views
            .iter()
            .map(|entry| entry.value().clone())
            .find(|view| view.user_id == user_id && view.module_id == module_id && view.is_default)
            .ok_or_else(|| {
                UnifiedError::not_found("No default view for this user and module")
            })
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
                .with_roles(vec![role.to_string()])
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_company("company-1".to_string()),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
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
    use crate::modules::saved_views::SavedViewsModule;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied), following the
    /// properties module's handler-test pattern.
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
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        ctx.modules
            .register(Arc::new(SavedViewsModule::new()))
            .expect("register saved_views module");
        web::Data::new(Arc::new(ctx))
    }

    fn test_state() -> web::Data<Arc<SavedViewsState>> {
        web::Data::new(Arc::new(SavedViewsState::new()))
    }

    fn request(user_id: &str) -> HttpRequest {
        verified_request(user_id, "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn seed_view(
        ctx: &AppContext,
        state: &SavedViewsState,
        user_id: &str,
        visibility: &str,
    ) -> SavedView {
        state
            .create(
                ctx,
                CreateSavedView {
                    user_id: user_id.to_string(),
                    module_id: "saved_views".to_string(),
                    name: "Open tickets".to_string(),
                    filter: json!({"status": "open"}),
                    sort: None,
                    order: Some("asc".to_string()),
                    visibility: Some(visibility.to_string()),
                    is_default: Some(false),
                },
            )
            .expect("seed view")
    }

    fn list_query(user_id: &str) -> web::Query<PaginationParams> {
        // A spoofed `?user_id=` filter naming the victim.
        let mut filters = std::collections::HashMap::new();
        filters.insert("user_id".to_string(), user_id.to_string());
        web::Query(PaginationParams {
            filters,
            ..Default::default()
        })
    }

    fn empty_query() -> web::Query<PaginationParams> {
        web::Query(PaginationParams::default())
    }

    #[tokio::test]
    async fn list_views_rejects_anonymous_access() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW]);
        let state = test_state();
        let resp = list_views(
            ctx.clone(),
            state,
            anonymous_request(),
            empty_query(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_views_ignores_spoofed_user_id_query_param() {
        // user-1 owns a private view; user-2 lists with
        // `?user_id=user-1`. The spoofed filter must not disclose the
        // victim's private view: the list scopes to the caller.
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        seed_view(&ctx, &state, "user-1", "private");
        let resp = list_views(
            ctx.clone(),
            state.clone(),
            request("user-2"),
            list_query("user-1"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        // The victim's private view must not be in the caller's list.
        let body = actix_web::test::read_body(resp).await;
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body["data"]["pagination"]["total"].as_u64(), Some(0));
    }

    #[tokio::test]
    async fn list_views_returns_own_views() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        seed_view(&ctx, &state, "user-1", "private");
        seed_view(&ctx, &state, "user-1", "shared");
        seed_view(&ctx, &state, "user-2", "private");
        let resp = list_views(ctx.clone(), state, request("user-1"), empty_query()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = actix_web::test::read_body(resp).await;
        let body: Value = serde_json::from_slice(&body).expect("json body");
        // Own private + own shared = 2; the other user's private
        // view is not listed.
        assert_eq!(body["data"]["pagination"]["total"].as_u64(), Some(2));
    }

    #[tokio::test]
    async fn get_view_rejects_anonymous_access() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let resp = get_view_handler(
            ctx,
            state,
            anonymous_request(),
            web::Path::from(view.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_view_rejects_cross_user_private_view() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let resp = get_view_handler(
            ctx,
            state,
            request("user-2"),
            web::Path::from(view.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_view_allows_owner_and_shared_for_others() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let private = seed_view(&ctx, &state, "user-1", "private");
        let own = get_view_handler(
            ctx.clone(),
            state.clone(),
            request("user-1"),
            web::Path::from(private.id),
        )
        .await;
        assert_eq!(own.status(), StatusCode::OK);
        let shared = seed_view(&ctx, &state, "user-1", "shared");
        let other = get_view_handler(
            ctx,
            state,
            request("user-2"),
            web::Path::from(shared.id),
        )
        .await;
        assert_eq!(other.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_view_ignores_body_user_id_and_uses_caller() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let body = web::Json(CreateSavedView {
            // The body claims to be user-1, but the caller is user-2.
            user_id: "user-1".to_string(),
            module_id: "saved_views".to_string(),
            name: "Impersonated".to_string(),
            filter: json!({"status": "open"}),
            sort: None,
            order: None,
            visibility: None,
            is_default: None,
        });
        let resp = create_view(ctx.clone(), state.clone(), request("user-2"), body).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let stored = state.views.iter().next().expect("one view").value().clone();
        assert_eq!(stored.user_id, test_user_uuid("user-2").to_string());
    }

    #[tokio::test]
    async fn create_view_rejects_anonymous_access() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let body = web::Json(CreateSavedView {
            user_id: "user-1".to_string(),
            module_id: "saved_views".to_string(),
            name: "Nope".to_string(),
            filter: json!({"status": "open"}),
            sort: None,
            order: None,
            visibility: None,
            is_default: None,
        });
        let resp = create_view(ctx, state, anonymous_request(), body).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_view_rejects_cross_user_access() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let body = web::Json(UpdateSavedView {
            name: Some("Hijacked".to_string()),
            filter: None,
            sort: None,
            order: None,
            visibility: Some("shared".to_string()),
            is_default: Some(true),
        });
        let resp = update_view(
            ctx.clone(),
            state.clone(),
            request("user-2"),
            web::Path::from(view.id.clone()),
            body,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let stored = get_view(&state, &view.id).expect("view still exists");
        assert_eq!(stored.name, "Open tickets");
        assert_eq!(stored.visibility, "private");
        assert!(!stored.is_default);
    }

    #[tokio::test]
    async fn delete_view_rejects_cross_user_access() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let resp = delete_view(
            ctx,
            state.clone(),
            request("user-2"),
            web::Path::from(view.id.clone()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(state.views.get(&view.id).is_some());
    }

    #[tokio::test]
    async fn set_default_view_rejects_cross_user_access() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let resp = set_default_view(
            ctx,
            state.clone(),
            request("user-2"),
            web::Path::from(view.id.clone()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(!state.views.get(&view.id).unwrap().is_default);
    }

    #[tokio::test]
    async fn apply_view_rejects_cross_user_private_view() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let resp = apply_view(
            ctx,
            state,
            request("user-2"),
            web::Path::from(view.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_default_view_ignores_spoofed_user_id_query_param() {
        // user-1 has a default view; user-2 asks with
        // `?user_id=user-1`. The spoofed id is ignored, so user-2
        // gets "no default view" instead of user-1's view.
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let made_default = set_default_view(
            ctx.clone(),
            state.clone(),
            request("user-1"),
            web::Path::from(view.id),
        )
        .await;
        assert_eq!(made_default.status(), StatusCode::OK);
        let mut filters = std::collections::HashMap::new();
        filters.insert("user_id".to_string(), "user-1".to_string());
        filters.insert("module_id".to_string(), "saved_views".to_string());
        let query = web::Query(PaginationParams {
            filters,
            ..Default::default()
        });
        let resp = get_default_view(ctx, state, request("user-2"), query).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_default_view_returns_own_default() {
        let ctx = test_ctx(&[PERM_SAVED_VIEWS_VIEW, PERM_SAVED_VIEWS_MANAGE]);
        let state = test_state();
        let view = seed_view(&ctx, &state, "user-1", "private");
        let made_default = set_default_view(
            ctx.clone(),
            state.clone(),
            request("user-1"),
            web::Path::from(view.id.clone()),
        )
        .await;
        assert_eq!(made_default.status(), StatusCode::OK);
        let mut filters = std::collections::HashMap::new();
        filters.insert("module_id".to_string(), "saved_views".to_string());
        let query = web::Query(PaginationParams {
            filters,
            ..Default::default()
        });
        let resp = get_default_view(ctx, state, request("user-1"), query).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = actix_web::test::read_body(resp).await;
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body["data"]["id"].as_str(), Some(view.id.as_str()));
    }
}
