//! Content Management module. — Tag handlers (task 356).
//!
//! Tags are a free-form taxonomy usable by built-in posts AND
//! custom entries. Attachment goes through the `post_tags` join
//! (`PostTagLink`), keyed by
//! `(tenant_id, target_kind, target_id, tag_id)`; the tag ids are
//! never embedded on the post or entry row (custom entries keep a
//! read snapshot, `CustomEntry::tag_ids`, updated together with
//! the join).
//!
//! C3 guards: every handler resolves the tenant first
//! (`require_tenant`), checks the caller company against it, and
//! requires a capability (`content.view` to read, `content.create`
//! to create, `content.manage` to update, delete, or (un)assign).
//! All tag reads and writes filter on the resolved tenant id, so a
//! tenant can never see or touch another tenant's tags.

use super::ai_capabilities::*;
use super::categories::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::state::*;
use super::types::*;

use actix_web::web;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared join helpers (also used by `custom_types.rs`).
// ---------------------------------------------------------------------------

/// All tags attached to one target, in tag-name order.
pub(super) fn tags_for_target(
    state: &ContentState,
    tenant_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Vec<Tag> {
    let mut tags: Vec<Tag> = state
        .post_tags
        .iter()
        .filter(|entry| {
            let link = entry.value();
            link.tenant_id == tenant_id
                && link.target_kind == target_kind
                && link.target_id == target_id
        })
        .filter_map(|entry| {
            state
                .tags
                .get(&entry.value().tag_id)
                .map(|tag| tag.value().clone())
        })
        .collect();
    tags.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    tags
}

/// The tag ids attached to one target, in tag-name order.
pub(super) fn tag_ids_for_target(
    state: &ContentState,
    tenant_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Vec<Uuid> {
    tags_for_target(state, tenant_id, target_kind, target_id)
        .into_iter()
        .map(|tag| tag.id)
        .collect()
}

/// Attach tags to a target. Every id must name a tag of this tenant
/// (unknown or foreign-tenant ids are 400, never silently skipped);
/// already-attached tags are ignored. Returns the attached tags.
pub(super) fn assign_tags_to_target(
    state: &ContentState,
    tenant_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    tag_ids: &[Uuid],
) -> Result<Vec<Tag>, UnifiedError> {
    let mut attached = Vec::new();
    let now = Utc::now();
    for tag_id in tag_ids {
        let tag = state.tags.get(tag_id).map(|entry| entry.value().clone());
        match tag {
            Some(tag) if tag.tenant_id == tenant_id => {
                state.post_tags.insert(
                    (tenant_id, target_kind.to_string(), target_id, tag.id),
                    PostTagLink {
                        tenant_id,
                        target_kind: target_kind.to_string(),
                        target_id,
                        tag_id: tag.id,
                        created_at: now,
                    },
                );
                attached.push(tag);
            }
            _ => {
                return Err(UnifiedError::bad_request(format!(
                    "tag {tag_id} does not exist"
                )));
            }
        }
    }
    Ok(attached)
}

/// Replace a target's whole tag assignment with `tag_ids`
/// (validated first, so a bad id leaves the old assignment
/// untouched). Used by custom-entry update.
pub(super) fn replace_tags_for_target(
    state: &ContentState,
    tenant_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    tag_ids: &[Uuid],
) -> Result<Vec<Tag>, UnifiedError> {
    // Validate every id before touching the join.
    let mut tags = Vec::new();
    for tag_id in tag_ids {
        match state.tags.get(tag_id).map(|entry| entry.value().clone()) {
            Some(tag) if tag.tenant_id == tenant_id => tags.push(tag),
            _ => {
                return Err(UnifiedError::bad_request(format!(
                    "tag {tag_id} does not exist"
                )));
            }
        }
    }
    remove_all_tags_for_target(state, tenant_id, target_kind, target_id);
    let now = Utc::now();
    for tag in &tags {
        state.post_tags.insert(
            (tenant_id, target_kind.to_string(), target_id, tag.id),
            PostTagLink {
                tenant_id,
                target_kind: target_kind.to_string(),
                target_id,
                tag_id: tag.id,
                created_at: now,
            },
        );
    }
    Ok(tags)
}

/// Detach one tag from a target. Returns false when nothing was
/// attached (the caller turns that into a 404).
pub(super) fn remove_tag_from_target(
    state: &ContentState,
    tenant_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    tag_id: Uuid,
) -> bool {
    state
        .post_tags
        .remove(&(tenant_id, target_kind.to_string(), target_id, tag_id))
        .is_some()
}

/// Detach every tag from a target (post/entry delete cleanup).
pub(super) fn remove_all_tags_for_target(
    state: &ContentState,
    tenant_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) {
    let keys: Vec<(Uuid, String, Uuid, Uuid)> = state
        .post_tags
        .iter()
        .filter(|entry| {
            let link = entry.value();
            link.tenant_id == tenant_id
                && link.target_kind == target_kind
                && link.target_id == target_id
        })
        .map(|entry| entry.key().clone())
        .collect();
    for key in keys {
        state.post_tags.remove(&key);
    }
}

/// True when any target in this tenant still carries the tag.
pub(super) fn tag_is_used(state: &ContentState, tenant_id: Uuid, tag_id: Uuid) -> bool {
    state
        .post_tags
        .iter()
        .any(|entry| entry.value().tenant_id == tenant_id && entry.value().tag_id == tag_id)
}

/// Resolve the tag ids named by an assign body (one `tag_id`, a
/// `tag_ids` list, or both), de-duplicated, order preserved.
pub(super) fn assigned_tag_ids(input: &AssignTagsInput) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = Vec::new();
    if let Some(tag_id) = input.tag_id {
        ids.push(tag_id);
    }
    for tag_id in &input.tag_ids {
        if !ids.contains(tag_id) {
            ids.push(*tag_id);
        }
    }
    ids
}

// ---------------------------------------------------------------------------
// Tag CRUD.
// ---------------------------------------------------------------------------

pub(super) async fn list_tags(
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
    let mut items: Vec<Tag> = state
        .tags
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tag| tag.tenant_id == tenant.tenant_id)
        .filter(|tag| {
            if let Some(needle) = &search {
                return matches_search(&tag.name, needle)
                    || matches_search(&tag.slug, needle)
                    || tag
                        .description
                        .as_deref()
                        .is_some_and(|description| matches_search(description, needle));
            }
            true
        })
        .collect();
    items = sort_tags(items, &params);
    api_json::success_response(paginate(items, &params).to_json(), 200)
}

pub(super) async fn create_tag(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreateTagInput>,
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
    let outcome = (|| -> Result<Tag, UnifiedError> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        if name.chars().count() > 200 {
            return Err(UnifiedError::bad_request(
                "name must be at most 200 characters",
            ));
        }
        if let Some(description) = &input.description {
            if description.chars().count() > 1000 {
                return Err(UnifiedError::bad_request(
                    "description must be at most 1000 characters",
                ));
            }
        }
        let slug = resolve_slug(input.slug, name, &|candidate| {
            state.tags.iter().any(|entry| {
                entry.value().tenant_id == tenant.tenant_id && entry.value().slug == candidate
            })
        })?;
        let now = Utc::now();
        let tag = Tag {
            id: Uuid::now_v7(),
            tenant_id: tenant.tenant_id,
            name: name.to_string(),
            slug,
            description: input.description,
            created_at: now,
            updated_at: now,
        };
        state.tags.insert(tag.id, tag.clone());
        Ok(tag)
    })();
    match outcome {
        Ok(tag) => api_json::success_response(tag, 201),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn get_tag(
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
    match state.tags.get(&id.into_inner()) {
        Some(tag) if tag.value().tenant_id == tenant.tenant_id => {
            api_json::success_response(tag.value().clone(), 200)
        }
        _ => api_json::error_response(&UnifiedError::not_found("tag not found"), ctx.is_debug()),
    }
}

pub(super) async fn update_tag(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<UpdateTagInput>,
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
    let outcome = (|| -> Result<Tag, UnifiedError> {
        let mut tag = state
            .tags
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|tag| tag.tenant_id == tenant.tenant_id)
            .ok_or_else(|| UnifiedError::not_found("tag not found"))?;
        if let Some(name) = &input.name {
            let name = name.trim();
            if name.is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            if name.chars().count() > 200 {
                return Err(UnifiedError::bad_request(
                    "name must be at most 200 characters",
                ));
            }
            tag.name = name.to_string();
        }
        if let Some(slug) = input.slug {
            let base = slugify(&slug);
            if base.is_empty() {
                return Err(UnifiedError::bad_request(
                    "slug must contain at least one letter or digit",
                ));
            }
            tag.slug = unique_slug(&base, &|candidate| {
                state.tags.iter().any(|entry| {
                    entry.value().tenant_id == tenant.tenant_id
                        && entry.value().slug == candidate
                        && entry.key() != &id
                })
            });
        }
        if let Some(description) = input.description {
            if description.chars().count() > 1000 {
                return Err(UnifiedError::bad_request(
                    "description must be at most 1000 characters",
                ));
            }
            tag.description = Some(description);
        }
        tag.updated_at = Utc::now();
        state.tags.insert(id, tag.clone());
        Ok(tag)
    })();
    match outcome {
        Ok(tag) => api_json::success_response(tag, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn delete_tag(
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
    match state.tags.get(&id).map(|entry| entry.value().clone()) {
        Some(tag) if tag.tenant_id == tenant.tenant_id => {
            if tag_is_used(&state, tenant.tenant_id, id) {
                return api_json::error_response(
                    &UnifiedError::conflict(
                        "tag is still assigned to one or more posts or entries",
                    ),
                    ctx.is_debug(),
                );
            }
            state.tags.remove(&id);
            api_json::success_response(json!({ "id": id, "deleted": true }), 200)
        }
        _ => api_json::error_response(&UnifiedError::not_found("tag not found"), ctx.is_debug()),
    }
}

// ---------------------------------------------------------------------------
// Tags on a post.
// ---------------------------------------------------------------------------

pub(super) async fn list_post_tags(
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
    let id = id.into_inner();
    if !state.posts.contains_key(&id) {
        return api_json::error_response(
            &UnifiedError::not_found("post not found"),
            ctx.is_debug(),
        );
    }
    api_json::success_response(
        tags_for_target(&state, tenant.tenant_id, TAG_TARGET_POST, id),
        200,
    )
}

pub(super) async fn assign_post_tags(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<AssignTagsInput>,
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
    if !state.posts.contains_key(&id) {
        return api_json::error_response(
            &UnifiedError::not_found("post not found"),
            ctx.is_debug(),
        );
    }
    let tag_ids = assigned_tag_ids(&input.into_inner());
    if tag_ids.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("provide tag_id or tag_ids"),
            ctx.is_debug(),
        );
    }
    match assign_tags_to_target(&state, tenant.tenant_id, TAG_TARGET_POST, id, &tag_ids) {
        Ok(_) => api_json::success_response(
            tags_for_target(&state, tenant.tenant_id, TAG_TARGET_POST, id),
            200,
        ),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn unassign_post_tag(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
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
    let (post_id, tag_id) = path.into_inner();
    if !state.posts.contains_key(&post_id) {
        return api_json::error_response(
            &UnifiedError::not_found("post not found"),
            ctx.is_debug(),
        );
    }
    if remove_tag_from_target(&state, tenant.tenant_id, TAG_TARGET_POST, post_id, tag_id) {
        api_json::success_response(
            tags_for_target(&state, tenant.tenant_id, TAG_TARGET_POST, post_id),
            200,
        )
    } else {
        api_json::error_response(
            &UnifiedError::not_found("tag is not assigned to this post"),
            ctx.is_debug(),
        )
    }
}

#[cfg(test)]
mod tests {
    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role.
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

    /// Deterministic test-user identity (same shape as the posts
    /// tests): distinct names map to distinct UUIDs.
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
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

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

    fn stored_post(state: &web::Data<ContentState>) -> Uuid {
        let now = Utc::now();
        let post = Post {
            id: Uuid::now_v7(),
            title: "Hello".to_string(),
            slug: "hello".to_string(),
            content: "Body".to_string(),
            excerpt: "Body".to_string(),
            status: ContentStatus::Draft,
            category_id: None,
            author_id: None,
            attachments: Vec::new(),
            created_at: now,
            updated_at: now,
            published_at: None,
        };
        state.posts.insert(post.id, post.clone());
        post.id
    }

    async fn created_tag(state: &web::Data<ContentState>, req: HttpRequest, name: &str) -> Uuid {
        // The request carries the tenant: every call in one test
        // must reuse the SAME request (clone), or each fresh
        // request resolves a different random tenant and the
        // per-tenant slug/tag lookups stop matching.
        let response = create_tag(
            state.clone(),
            data(allowing_context()),
            req,
            web::Json(CreateTagInput {
                name: name.to_string(),
                slug: None,
                description: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        state
            .tags
            .iter()
            .next()
            .expect("tag stored")
            .key()
            .to_owned()
    }

    #[tokio::test]
    async fn list_tags_rejects_anonymous_callers() {
        let response = list_tags(
            state(),
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_tags_rejects_callers_without_content_view() {
        let response = list_tags(
            state(),
            data(test_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_tag_derives_a_kebab_slug() {
        let state = state();
        let req = authed_request();
        let tag_id = created_tag(&state, req.clone(), "Breaking News").await;
        let tag = state.tags.get(&tag_id).expect("tag stored").value().clone();
        assert_eq!(tag.slug, "breaking-news");
        // A second tag with the same name gets a unique slug.
        created_tag(&state, req.clone(), "Breaking News").await;
        let slugs: Vec<String> = state
            .tags
            .iter()
            .map(|entry| entry.value().slug.clone())
            .collect();
        assert!(slugs.contains(&"breaking-news".to_string()));
        assert!(slugs.contains(&"breaking-news-2".to_string()));
    }

    #[tokio::test]
    async fn delete_tag_conflicts_while_assigned() {
        let state = state();
        let req = authed_request();
        let post_id = stored_post(&state);
        let tag_id = created_tag(&state, req.clone(), "News").await;
        let response = assign_post_tags(
            state.clone(),
            data(allowing_context()),
            req.clone(),
            web::Path::from(post_id),
            web::Json(AssignTagsInput {
                tag_id: Some(tag_id),
                tag_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = delete_tag(
            state.clone(),
            data(allowing_context()),
            req.clone(),
            web::Path::from(tag_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        // Unassign, then the delete succeeds.
        let response = unassign_post_tag(
            state.clone(),
            data(allowing_context()),
            req.clone(),
            web::Path::from((post_id, tag_id)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = delete_tag(
            state.clone(),
            data(allowing_context()),
            req.clone(),
            web::Path::from(tag_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn assign_post_tags_rejects_unknown_tags() {
        let state = state();
        let req = authed_request();
        let post_id = stored_post(&state);
        let response = assign_post_tags(
            state.clone(),
            data(allowing_context()),
            req.clone(),
            web::Path::from(post_id),
            web::Json(AssignTagsInput {
                tag_id: Some(Uuid::now_v7()),
                tag_ids: Vec::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(state.post_tags.is_empty());
    }

    #[tokio::test]
    async fn delete_post_detaches_its_tags() {
        let state = state();
        let req = authed_request();
        let post_id = stored_post(&state);
        let tag_id = created_tag(&state, req.clone(), "News").await;
        assign_post_tags(
            state.clone(),
            data(allowing_context()),
            req.clone(),
            web::Path::from(post_id),
            web::Json(AssignTagsInput {
                tag_id: Some(tag_id),
                tag_ids: Vec::new(),
            }),
        )
        .await;
        assert!(!state.post_tags.is_empty());
        let response = delete_post(
            state.clone(),
            data(allowing_context()),
            req.clone(),
            web::Path::from(post_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.post_tags.is_empty());
        // The tag itself survives the post delete.
        assert!(state.tags.contains_key(&tag_id));
    }
}
