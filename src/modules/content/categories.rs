//! Content Management module. - Category handlers.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::PaginationParams;
use actix_web::web;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_categories(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let search = params.search.clone();
    let mut items: Vec<Category> = state
        .categories
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|category| {
            if let Some(needle) = &search {
                return matches_search(&category.name, needle)
                    || matches_search(&category.slug, needle)
                    || category
                        .description
                        .as_deref()
                        .is_some_and(|description| matches_search(description, needle));
            }
            true
        })
        .collect();
    items = sort_categories(items, &params);
    api_json::success_response(paginate(items, &params).to_json(), 200)
}

pub(super) async fn create_category(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreateCategoryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_CREATE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    let outcome = (|| -> Result<Category, UnifiedError> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        if name.chars().count() > 200 {
            return Err(UnifiedError::bad_request(
                "name must be at most 200 characters",
            ));
        }
        let slug = resolve_slug(input.slug, name, &|candidate| {
            state
                .categories
                .iter()
                .any(|entry| entry.value().slug == candidate)
        })?;
        let now = Utc::now();
        let category = Category {
            id: Uuid::now_v7(),
            name: name.to_string(),
            slug,
            description: input.description,
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        Ok(category)
    })();
    match outcome {
        Ok(category) => api_json::success_response(category, 201),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn get_category(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    match state.categories.get(&id.into_inner()) {
        Some(category) => api_json::success_response(category.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("category not found"), ctx.is_debug()),
    }
}

pub(super) async fn update_category(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<UpdateCategoryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let id = id.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<Category, UnifiedError> {
        let mut category = state
            .categories
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("category not found"))?;
        if let Some(name) = &input.name {
            let name = name.trim();
            if name.is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            category.name = name.to_string();
        }
        if let Some(slug) = input.slug {
            let base = slugify(&slug);
            if base.is_empty() {
                return Err(UnifiedError::bad_request(
                    "slug must contain at least one letter or digit",
                ));
            }
            category.slug = unique_slug(&base, &|candidate| {
                state.categories.iter().any(|entry| {
                    entry.value().slug == candidate && entry.key() != &id
                })
            });
        }
        if input.description.is_some() {
            category.description = input.description;
        }
        category.updated_at = Utc::now();
        state.categories.insert(id, category.clone());
        Ok(category)
    })();
    match outcome {
        Ok(category) => api_json::success_response(category, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn delete_category(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let id = id.into_inner();
    if state
        .posts
        .iter()
        .any(|entry| entry.value().category_id == Some(id))
        || state.custom_entries.iter().any(|entry| {
            entry.value().tenant_id == tenant.tenant_id && entry.value().category_id == Some(id)
        })
    {
        return api_json::error_response(
            &UnifiedError::conflict("category is still used by one or more posts or entries"),
            ctx.is_debug(),
        );
    }
    match state.categories.remove(&id) {
        Some((_, category)) => api_json::success_response(
            json!({ "id": category.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("category not found"), ctx.is_debug()),
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
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

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

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn state() -> web::Data<ContentState> {
        web::Data::new(ContentState::default())
    }

    #[tokio::test]
    async fn list_categories_rejects_anonymous_callers() {
        let response = list_categories(
            state(),
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_categories_accepts_callers_with_content_view() {
        let response = list_categories(
            state(),
            data(allowing_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_category_rejects_anonymous_callers() {
        let input = CreateCategoryInput {
            name: "News".to_string(),
            slug: None,
            description: None,
        };
        let response = create_category(
            state(),
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Json(input),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_category_accepts_callers_with_content_create() {
        let input = CreateCategoryInput {
            name: "News".to_string(),
            slug: None,
            description: None,
        };
        let response = create_category(
            state(),
            data(allowing_context()),
            authed_request(),
            web::Json(input),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn delete_category_rejects_callers_without_content_manage() {
        let response = delete_category(
            state(),
            data(test_context()),
            authed_request(),
            web::Path::from(Uuid::now_v7()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
