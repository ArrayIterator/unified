//! Content Management module. — Post handlers.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::categories::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::state::*;
use super::tags::*;
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

pub(super) async fn list_posts(
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
        return api_json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let search = params.search.clone();
    let status_filter = match params
        .filters
        .get("status")
        .map(|raw| parse_status_filter(raw))
    {
        Some(Ok(status)) => Some(status),
        Some(Err(error)) => return api_json::error_response(&error, debug),
        None => None,
    };
    let category_filter = params.filters.get("category_id").cloned();
    let author_filter = params.filters.get("author_id").cloned();
    // A `tag` filter names a tag id or slug; an unknown tag matches
    // nothing (same as the category filter above).
    let tag_filter = params.filters.get("tag").cloned();
    let tag_ids: Vec<Uuid> = match &tag_filter {
        Some(raw) => state
            .tags
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == tenant.tenant_id
                    && (entry.value().id.to_string() == *raw || entry.value().slug == *raw)
            })
            .map(|entry| *entry.key())
            .collect(),
        None => Vec::new(),
    };
    let mut items: Vec<Post> = state
        .posts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|post| {
            if let Some(wanted) = &status_filter {
                if post.status != *wanted {
                    return false;
                }
            }
            if let Some(category_id) = &category_filter {
                if post.category_id.map(|id| id.to_string()) != Some(category_id.clone()) {
                    return false;
                }
            }
            if let Some(author_id) = &author_filter {
                if post.author_id.map(|id| id.to_string()) != Some(author_id.clone()) {
                    return false;
                }
            }
            if tag_filter.is_some() {
                let attached = tag_ids_for_target(&state, tenant.tenant_id, TAG_TARGET_POST, post.id);
                if !tag_ids.iter().any(|tag_id| attached.contains(tag_id)) {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !matches_search(&post.title, needle)
                    && !matches_search(&post.content, needle)
                    && !matches_search(&post.slug, needle)
                {
                    return false;
                }
            }
            true
        })
        .collect();
    items = sort_posts(items, &params);
    api_json::success_response(paginate(items, &params).to_json(), 200)
}

pub(super) async fn create_post(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreatePostInput>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let actor = match caller(&req) {
        Ok(actor) => actor,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &actor.role, PERM_CONTENT_CREATE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    // The recorded author is the header caller: a body `author_id`
    // naming someone else is forgery unless the caller holds
    // `content.manage` (admin/on-behalf flow).
    let author_id = match resolve_body_author(input.author_id, &actor.actor, &ctx, &actor.role) {
        Ok(author_id) => author_id,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let outcome = (|| -> Result<Post, UnifiedError> {
        let title = validate_title(&input.title)?;
        let content = validate_content(input.content)?;
        if let Some(category_id) = input.category_id {
            if !state.categories.contains_key(&category_id) {
                return Err(UnifiedError::bad_request(format!(
                    "category {category_id} does not exist"
                )));
            }
        }
        let slug = resolve_slug(input.slug, &title, &|candidate| {
            state
                .posts
                .iter()
                .any(|entry| entry.value().slug == candidate)
        })?;
        let now = Utc::now();
        let status = input.status.unwrap_or_default();
        let published_at = if status == ContentStatus::Published {
            Some(now)
        } else {
            None
        };
        let post = Post {
            id: Uuid::now_v7(),
            excerpt: make_excerpt(&content, input.excerpt.as_deref()),
            title,
            slug,
            content,
            status,
            category_id: input.category_id,
            author_id,
            attachments: input.attachments,
            created_at: now,
            updated_at: now,
            published_at,
        };
        state.posts.insert(post.id, post.clone());
        Ok(post)
    })();
    match outcome {
        Ok(post) => {
            if post.status == ContentStatus::Published {
                emit_published(
                    &ctx,
                    "post",
                    &post.id,
                    &post.title,
                    &post.slug,
                    &post.content,
                    tenant.tenant_id,
                    &post.attachments,
                );
            }
            api_json::success_response(post, 201)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn get_post(
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
    match state.posts.get(&id.into_inner()) {
        Some(post) => api_json::success_response(post.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("post not found"), ctx.is_debug()),
    }
}

pub(super) async fn update_post(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<UpdatePostInput>,
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
    let outcome = (|| -> Result<(Post, Option<bool>), UnifiedError> {
        let mut post = state
            .posts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("post not found"))?;
        // `Some(true)` = newly published, `Some(false)` = newly a
        // draft, `None` = no status change.
        let mut transition: Option<bool> = None;
        if let Some(title) = &input.title {
            post.title = validate_title(title)?;
        }
        if let Some(content) = &input.content {
            post.content = validate_content(Some(content.clone()))?;
        }
        if let Some(category_id) = input.category_id {
            if !state.categories.contains_key(&category_id) {
                return Err(UnifiedError::bad_request(format!(
                    "category {category_id} does not exist"
                )));
            }
            post.category_id = Some(category_id);
        }
        if input.author_id.is_some() {
            post.author_id = input.author_id;
        }
        if let Some(slug) = input.slug {
            let base = slugify(&slug);
            if base.is_empty() {
                return Err(UnifiedError::bad_request(
                    "slug must contain at least one letter or digit",
                ));
            }
            post.slug = unique_slug(&base, &|candidate| {
                state.posts.iter().any(|entry| {
                    entry.value().slug == candidate && entry.key() != &id
                })
            });
        }
        if let Some(excerpt) = input.excerpt.as_deref() {
            post.excerpt = make_excerpt(&post.content, Some(excerpt));
        } else if input.content.is_some() {
            // Regenerate the excerpt when the body changed and no explicit
            // excerpt was supplied.
            post.excerpt = make_excerpt(&post.content, None);
        }
        // Attachments replace wholesale when the field is present
        // (absent = leave them alone); they reference the shared
        // documents store, never a re-upload.
        if let Some(attachments) = input.attachments {
            post.attachments = attachments;
        }
        if let Some(status) = input.status {
            if status != post.status {
                if status == ContentStatus::Published {
                    post.published_at = Some(Utc::now());
                    transition = Some(true);
                } else {
                    post.published_at = None;
                    transition = Some(false);
                }
                post.status = status;
            }
        }
        post.updated_at = Utc::now();
        state.posts.insert(id, post.clone());
        Ok((post, transition))
    })();
    match outcome {
        Ok((post, transition)) => {
            match transition {
                Some(true) => emit_published(
                    &ctx,
                    "post",
                    &post.id,
                    &post.title,
                    &post.slug,
                    &post.content,
                    tenant.tenant_id,
                    &post.attachments,
                ),
                Some(false) => emit_unpublished(
                    &ctx,
                    "post",
                    &post.id,
                    &post.title,
                    &post.slug,
                    tenant.tenant_id,
                    &post.attachments,
                ),
                None => {}
            }
            api_json::success_response(post, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn delete_post(
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
    match state.posts.remove(&id) {
        Some((_, post)) => {
            // Detach the post's tags with it (the join must not
            // outlive its target).
            remove_all_tags_for_target(&state, tenant.tenant_id, TAG_TARGET_POST, id);
            api_json::success_response(
                json!({ "id": post.id, "deleted": true }),
                200,
            )
        }
        None => api_json::error_response(&UnifiedError::not_found("post not found"), ctx.is_debug()),
    }
}

pub(super) async fn publish_post(
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
    let outcome = (|| -> Result<Post, UnifiedError> {
        let mut post = state
            .posts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("post not found"))?;
        if post.status == ContentStatus::Published {
            return Err(UnifiedError::conflict("post is already published"));
        }
        post.status = ContentStatus::Published;
        post.published_at = Some(Utc::now());
        post.updated_at = Utc::now();
        state.posts.insert(id, post.clone());
        Ok(post)
    })();
    match outcome {
        Ok(post) => {
            emit_published(
                &ctx,
                "post",
                &post.id,
                &post.title,
                &post.slug,
                &post.content,
                tenant.tenant_id,
                &post.attachments,
            );
            api_json::success_response(post, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn unpublish_post(
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
    let outcome = (|| -> Result<Post, UnifiedError> {
        let mut post = state
            .posts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("post not found"))?;
        if post.status == ContentStatus::Draft {
            return Err(UnifiedError::conflict("post is already a draft"));
        }
        post.status = ContentStatus::Draft;
        post.published_at = None;
        post.updated_at = Utc::now();
        state.posts.insert(id, post.clone());
        Ok(post)
    })();
    match outcome {
        Ok(post) => {
            emit_unpublished(
                &ctx,
                "post",
                &post.id,
                &post.title,
                &post.slug,
                tenant.tenant_id,
                &post.attachments,
            );
            api_json::success_response(post, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

/// Emit the `content.published` hook for a newly published post or page.
/// The payload carries the tenant id and the post/page's media
/// attachments so the documents module can flip attached media
/// public (the flip listener reads `tenant_id` + `attachments`).
pub(super) fn emit_published(
    ctx: &AppContext,
    kind: &str,
    id: &Uuid,
    title: &str,
    slug: &str,
    content: &str,
    tenant_id: Uuid,
    attachments: &[PostAttachment],
) {
    ctx.hooks.emit(
        "content.published",
        json!({
            "kind": kind,
            "id": id.to_string(),
            "title": title,
            "slug": slug,
            "content": content,
            "published_at": Utc::now().to_rfc3339(),
            "tenant_id": tenant_id.to_string(),
            "attachments": attachments,
        }),
    );
}

/// Emit the `content.unpublished` hook for a post/page returned to
/// draft. Same shape as `content.published` (minus the body):
/// the documents module flips the attached media back private.
pub(super) fn emit_unpublished(
    ctx: &AppContext,
    kind: &str,
    id: &Uuid,
    title: &str,
    slug: &str,
    tenant_id: Uuid,
    attachments: &[PostAttachment],
) {
    ctx.hooks.emit(
        "content.unpublished",
        json!({
            "kind": kind,
            "id": id.to_string(),
            "title": title,
            "slug": slug,
            "tenant_id": tenant_id.to_string(),
            "attachments": attachments,
        }),
    );
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
    async fn list_posts_rejects_anonymous_callers() {
        let response = list_posts(
            state(),
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_posts_rejects_callers_without_content_view() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response = list_posts(
            state(),
            data(test_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_posts_accepts_callers_with_content_view() {
        let response = list_posts(
            state(),
            data(allowing_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_posts_rejects_invalid_status_filter() {
        let params = PaginationParams {
            filters: [("status".to_string(), "garbage".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let response = list_posts(
            state(),
            data(allowing_context()),
            authed_request(),
            web::Query(params),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_posts_accepts_valid_status_filters() {
        for status in ["published", "draft"] {
            let params = PaginationParams {
                filters: [("status".to_string(), status.to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            };
            let response = list_posts(
                state(),
                data(allowing_context()),
                authed_request(),
                web::Query(params),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "status {status}");
        }
    }

    #[tokio::test]
    async fn create_post_rejects_anonymous_callers() {
        let input = CreatePostInput {
            title: "Hello".to_string(),
            slug: None,
            content: None,
            excerpt: None,
            category_id: None,
            author_id: None,
            status: None,
        };
        let response = create_post(
            state(),
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Json(input),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_post_rejects_callers_without_content_manage() {
        let response = delete_post(
            state(),
            data(test_context()),
            authed_request(),
            web::Path::from(Uuid::now_v7()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn publish_post_rejects_anonymous_callers() {
        let response = publish_post(
            state(),
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Path::from(Uuid::now_v7()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// A context whose `role.check` handler allows exactly the
    /// given permissions (all others denied).
    fn scoped_context(allowed: &[&str]) -> AppContext {
        use unified_core::hooks::HookContext;
        let ctx = test_context();
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut HookContext| {
                let permission = context
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                context.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        ctx
    }

    fn request_as(actor: &str, role: &str) -> HttpRequest {
        verified_request(actor, role)
    }

    fn post_input(author_id: Option<Uuid>) -> CreatePostInput {
        CreatePostInput {
            title: "Hello".to_string(),
            slug: None,
            content: Some("Some body text".to_string()),
            excerpt: None,
            category_id: None,
            author_id,
            status: None,
        }
    }

    #[tokio::test]
    async fn create_post_rejects_forged_author() {
        let caller = Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("caller id");
        let other = Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("other id");
        let state = state();
        let response = create_post(
            state.clone(),
            data(scoped_context(&[PERM_CONTENT_CREATE])),
            request_as(&caller.to_string(), "editor"),
            web::Json(post_input(Some(other))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(state.posts.is_empty());
    }

    #[tokio::test]
    async fn create_post_records_the_caller_as_author() {
        let caller = Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("caller id");
        let state = state();
        let response = create_post(
            state.clone(),
            data(scoped_context(&[PERM_CONTENT_CREATE])),
            request_as(&caller.to_string(), "editor"),
            web::Json(post_input(Some(caller))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let stored = state
            .posts
            .iter()
            .next()
            .expect("post stored")
            .value()
            .clone();
        assert_eq!(stored.author_id, Some(caller));
    }

    #[tokio::test]
    async fn create_post_allows_manage_on_behalf() {
        let caller = Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("caller id");
        let other = Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("other id");
        let state = state();
        let response = create_post(
            state.clone(),
            data(scoped_context(&[PERM_CONTENT_CREATE, PERM_CONTENT_MANAGE])),
            request_as(&caller.to_string(), "admin"),
            web::Json(post_input(Some(other))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let stored = state
            .posts
            .iter()
            .next()
            .expect("post stored")
            .value()
            .clone();
        assert_eq!(stored.author_id, Some(other));
    }

    #[tokio::test]
    async fn create_post_without_author_records_no_author() {
        let state = state();
        let response = create_post(
            state.clone(),
            data(scoped_context(&[PERM_CONTENT_CREATE])),
            request_as("user-1", "editor"),
            web::Json(post_input(None)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let stored = state
            .posts
            .iter()
            .next()
            .expect("post stored")
            .value()
            .clone();
        assert_eq!(stored.author_id, None);
    }
}
