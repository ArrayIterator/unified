//! HTTP handlers for forums, categories, and the public routes.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    Caller, caller, can_manage, check_capability, created, err, forum_visible_to, load_category,
    load_forum, load_forum_by_slug, load_thread, ok, paginate, post_visible_to, require_permission,
    require_text, require_tenant, resolve_tenant, slugify, thread_view, valid_slug, vote_totals,
};
use super::state::ForumState;
use super::types::{
    CreateCategoryRequest, CreateForumRequest, Forum, ForumCategory, ForumVisibility,
    MAX_DESCRIPTION_LEN, MAX_NAME_LEN, PERM_FORUMS_MANAGE, PERM_FORUMS_VIEW, UpdateCategoryRequest,
    UpdateForumRequest,
};

/// Path carrying a forum id.
#[derive(Debug, Deserialize)]
struct ForumPath {
    forum_id: String,
}

/// Path carrying a forum id and a category id.
#[derive(Debug, Deserialize)]
struct CategoryPath {
    forum_id: String,
    category_id: String,
}

/// Query for the public search route: `q` plus paging.
#[derive(Debug, Deserialize)]
struct PublicSearchQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
}

fn pagination_from(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
    PaginationParams {
        page,
        per_page,
        sort: None,
        order: None,
        search: None,
        filters: std::collections::HashMap::new(),
    }
}

/// Resolve a slug from an optional client slug or a name: a
/// client slug is lowercased and validated, otherwise the name is
/// slugified. The returned slug is never empty.
fn resolve_slug(input: Option<&String>, name: &str) -> UnifiedResult<String> {
    match input {
        Some(raw) => {
            let slug = raw.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                ));
            }
            Ok(slug)
        }
        None => Ok(slugify(name)),
    }
}

/// Visibility gate for one forum on an authenticated route.
/// Members-only forums need the `forums.view` capability checked
/// here at the route layer, before the foundation helper is
/// called (the helper carries no role, so it cannot check the
/// capability itself). A forum the caller may not see reads as
/// 404, never 403, so its existence does not leak.
fn check_forum_visible(ctx: &AppContext, caller: &Caller, forum: &Forum) -> UnifiedResult<()> {
    let is_manager = can_manage(ctx, &caller.role);
    let visible = match forum.visibility {
        ForumVisibility::MembersOnly => {
            check_capability(ctx, &caller.role, PERM_FORUMS_VIEW)
                && forum_visible_to(forum, true, is_manager)
        }
        _ => forum_visible_to(forum, true, is_manager),
    };
    if visible {
        Ok(())
    } else {
        Err(UnifiedError::not_found(format!(
            "Forum not found: {}",
            forum.id
        )))
    }
}

/// Load a forum in this tenant and apply the visibility gate.
fn load_visible_forum(
    state: &ForumState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &Caller,
    forum_id: &str,
) -> UnifiedResult<Forum> {
    let forum = load_forum(state, tenant_id, forum_id)?;
    check_forum_visible(ctx, caller, &forum)?;
    Ok(forum)
}

/// Load a category in this tenant and confirm it belongs to the
/// given forum (which must already have passed the visibility
/// gate). A category of another forum reads as 404.
fn load_category_of_forum(
    state: &ForumState,
    tenant_id: &str,
    forum: &Forum,
    category_id: &str,
) -> UnifiedResult<ForumCategory> {
    let category = load_category(state, tenant_id, category_id)?;
    if category.forum_id != forum.id {
        return Err(UnifiedError::not_found(format!(
            "Category not found: {}",
            category.id
        )));
    }
    Ok(category)
}

// Forum handlers (authenticated)

pub(crate) async fn list_forums(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        // Members-only rows need the `forums.view` capability,
        // checked here at the route layer before the helper runs.
        let can_view_members = check_capability(&ctx, &caller.role, PERM_FORUMS_VIEW);
        let params = query.into_inner();
        let visibility_filter = params.filters.get("visibility").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Forum> = state
            .forums
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|forum| {
                if forum.tenant_id != tenant_id {
                    return false;
                }
                match forum.visibility {
                    ForumVisibility::Public => {}
                    ForumVisibility::MembersOnly => {
                        if !can_view_members {
                            return false;
                        }
                    }
                    ForumVisibility::StaffOnly => {
                        if !is_manager {
                            return false;
                        }
                    }
                }
                if let Some(visibility) = &visibility_filter {
                    if forum.visibility.as_str() != visibility {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        forum.name,
                        forum.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|forum| (forum.sort_order, forum.name.clone()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_forum(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateForumRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Forum> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = resolve_slug(input.slug.as_ref(), &name)?;
        if state
            .forums
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "slug is already in use: {slug}"
            )));
        }
        let now = Utc::now();
        let forum = Forum {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            slug,
            icon: input.icon,
            visibility: input.visibility.unwrap_or_default(),
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now,
            updated_at: now,
        };
        state.forums.insert(forum.id, forum.clone());
        state.emit(
            "forums.forum.created",
            json!({
                "tenant_id": tenant_id,
                "forum_id": forum.id.to_string(),
                "slug": forum.slug,
                "name": forum.name,
                "visibility": forum.visibility.as_str(),
            }),
        );
        Ok(forum)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_forum(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ForumPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Forum> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let forum = load_visible_forum(&state, &ctx, &tenant_id, &caller, &path.forum_id)?;
        Ok(forum)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_forum(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ForumPath>,
    body: web::Json<UpdateForumRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Forum> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MANAGE)?;
        let mut forum = load_forum(&state, &tenant_id, &path.forum_id)?;
        let input = body.into_inner();
        if let Some(raw) = &input.name {
            forum.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            forum.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.slug {
            let slug = resolve_slug(Some(raw), &forum.name)?;
            if slug != forum.slug
                && state
                    .forums
                    .iter()
                    .any(|entry| entry.value().tenant_id == tenant_id && entry.value().slug == slug)
            {
                return Err(UnifiedError::conflict(format!(
                    "slug is already in use: {slug}"
                )));
            }
            forum.slug = slug;
        }
        if let Some(icon) = &input.icon {
            forum.icon = icon.clone();
        }
        if let Some(visibility) = input.visibility {
            forum.visibility = visibility;
        }
        if let Some(sort_order) = input.sort_order {
            forum.sort_order = sort_order;
        }
        forum.updated_at = Utc::now();
        state.forums.insert(forum.id, forum.clone());
        state.emit(
            "forums.forum.updated",
            json!({
                "tenant_id": tenant_id,
                "forum_id": forum.id.to_string(),
                "slug": forum.slug,
                "name": forum.name,
                "visibility": forum.visibility.as_str(),
            }),
        );
        Ok(forum)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_forum(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ForumPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MANAGE)?;
        let forum = load_forum(&state, &tenant_id, &path.forum_id)?;
        if state
            .categories
            .iter()
            .any(|entry| entry.value().forum_id == forum.id)
        {
            return Err(UnifiedError::bad_request(
                "Forum still has categories; delete them first",
            ));
        }
        if state
            .threads
            .iter()
            .any(|entry| entry.value().forum_id == forum.id)
        {
            return Err(UnifiedError::bad_request(
                "Forum still has threads; delete them first",
            ));
        }
        state.forums.remove(&forum.id);
        state.emit(
            "forums.forum.deleted",
            json!({
                "tenant_id": tenant_id,
                "forum_id": forum.id.to_string(),
                "slug": forum.slug,
                "name": forum.name,
            }),
        );
        Ok(json!({ "deleted": true, "forum_id": forum.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Category handlers (authenticated, nested under a forum)

pub(crate) async fn list_forum_categories(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ForumPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let forum = load_visible_forum(&state, &ctx, &tenant_id, &caller, &path.forum_id)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ForumCategory> = state
            .categories
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|category| {
                if category.tenant_id != tenant_id || category.forum_id != forum.id {
                    return false;
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        category.name,
                        category.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|category| (category.sort_order, category.name.clone()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_forum_category(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ForumPath>,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ForumCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MANAGE)?;
        let forum = load_forum(&state, &tenant_id, &path.forum_id)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = resolve_slug(input.slug.as_ref(), &name)?;
        if state
            .categories
            .iter()
            .any(|entry| entry.value().forum_id == forum.id && entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "slug is already in use in this forum: {slug}"
            )));
        }
        let now = Utc::now();
        let category = ForumCategory {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            forum_id: forum.id,
            name,
            description,
            slug,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        state.emit(
            "forums.category.created",
            json!({
                "tenant_id": tenant_id,
                "forum_id": forum.id.to_string(),
                "category_id": category.id.to_string(),
                "slug": category.slug,
                "name": category.name,
            }),
        );
        Ok(category)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_category(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<CategoryPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ForumCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let forum = load_visible_forum(&state, &ctx, &tenant_id, &caller, &path.forum_id)?;
        let category = load_category_of_forum(&state, &tenant_id, &forum, &path.category_id)?;
        Ok(category)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_category(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<CategoryPath>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ForumCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MANAGE)?;
        let forum = load_forum(&state, &tenant_id, &path.forum_id)?;
        let mut category = load_category_of_forum(&state, &tenant_id, &forum, &path.category_id)?;
        let input = body.into_inner();
        if let Some(raw) = &input.name {
            category.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            category.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.slug {
            let slug = resolve_slug(Some(raw), &category.name)?;
            if slug != category.slug
                && state
                    .categories
                    .iter()
                    .any(|entry| entry.value().forum_id == forum.id && entry.value().slug == slug)
            {
                return Err(UnifiedError::conflict(format!(
                    "slug is already in use in this forum: {slug}"
                )));
            }
            category.slug = slug;
        }
        if let Some(sort_order) = input.sort_order {
            category.sort_order = sort_order;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        state.emit(
            "forums.category.updated",
            json!({
                "tenant_id": tenant_id,
                "forum_id": forum.id.to_string(),
                "category_id": category.id.to_string(),
                "slug": category.slug,
                "name": category.name,
            }),
        );
        Ok(category)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_category(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<CategoryPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MANAGE)?;
        let forum = load_forum(&state, &tenant_id, &path.forum_id)?;
        let category = load_category_of_forum(&state, &tenant_id, &forum, &path.category_id)?;
        if state
            .threads
            .iter()
            .any(|entry| entry.value().category_id == Some(category.id))
        {
            return Err(UnifiedError::bad_request(
                "Category still has threads; delete or move them first",
            ));
        }
        state.categories.remove(&category.id);
        state.emit(
            "forums.category.deleted",
            json!({
                "tenant_id": tenant_id,
                "forum_id": forum.id.to_string(),
                "category_id": category.id.to_string(),
                "slug": category.slug,
                "name": category.name,
            }),
        );
        Ok(json!({ "deleted": true, "category_id": category.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Public handlers (no caller; tenant resolution still applies)

pub(crate) async fn list_public_forums(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Forum> = state
            .forums
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|forum| {
                if forum.tenant_id != tenant_id || forum.visibility != ForumVisibility::Public {
                    return false;
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        forum.name,
                        forum.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|forum| (forum.sort_order, forum.name.clone()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_public_forum(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Forum> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let forum = load_forum_by_slug(&state, &tenant_id, &path.into_inner())?;
        if forum.visibility != ForumVisibility::Public {
            return Err(UnifiedError::not_found("Forum not found".to_string()));
        }
        Ok(forum)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_public_forum_threads(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let forum = load_forum_by_slug(&state, &tenant_id, &path.into_inner())?;
        if forum.visibility != ForumVisibility::Public {
            return Err(UnifiedError::not_found("Forum not found".to_string()));
        }
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| thread.tenant_id == tenant_id && thread.forum_id == forum.id)
            // thread_view counts only visible (non-hidden) posts
            // for an anonymous reader, so hidden posts never leak
            // through the public list.
            .map(|thread| thread_view(&state, &thread, "", false))
            .collect();
        items.sort_by(|a, b| {
            let pinned_a = a.get("pinned").and_then(Value::as_bool).unwrap_or(false);
            let pinned_b = b.get("pinned").and_then(Value::as_bool).unwrap_or(false);
            pinned_b.cmp(&pinned_a).then(
                b.get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&a.get("created_at").and_then(Value::as_str)),
            )
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_public_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let thread = load_thread(&state, &tenant_id, &path.into_inner())?;
        let forum = load_forum(&state, &tenant_id, &thread.forum_id.to_string())?;
        if forum.visibility != ForumVisibility::Public {
            return Err(UnifiedError::not_found(format!(
                "Thread not found: {}",
                thread.id
            )));
        }
        let params = query.into_inner();
        let mut posts: Vec<Value> = state
            .posts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|post| post.thread_id == thread.id && post_visible_to(post, "", false))
            .map(|post| {
                let mut value = serde_json::to_value(&post).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    let (up, down) = vote_totals(&state, post.id);
                    map.insert("up_votes".to_string(), Value::from(up));
                    map.insert("down_votes".to_string(), Value::from(down));
                    map.insert("score".to_string(), Value::from(up - down));
                }
                value
            })
            .collect();
        posts.sort_by(|a, b| {
            a.get("created_at")
                .and_then(Value::as_str)
                .cmp(&b.get("created_at").and_then(Value::as_str))
        });
        let page = paginate(posts, &params).to_json();
        let mut thread_value = thread_view(&state, &thread, "", false);
        if let Value::Object(map) = &mut thread_value {
            map.insert("posts".to_string(), page);
        }
        Ok(thread_value)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn public_search(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PublicSearchQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let input = query.into_inner();
        let needle = input.q.unwrap_or_default().trim().to_lowercase();
        if needle.is_empty() {
            return Err(UnifiedError::bad_request("q must not be empty"));
        }
        let params = pagination_from(input.page, input.per_page);
        let public_forum_ids: Vec<Uuid> = state
            .forums
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|forum| {
                forum.tenant_id == tenant_id && forum.visibility == ForumVisibility::Public
            })
            .map(|forum| forum.id)
            .collect();
        let mut items: Vec<Value> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| {
                if thread.tenant_id != tenant_id || !public_forum_ids.contains(&thread.forum_id) {
                    return false;
                }
                if thread.title.to_lowercase().contains(&needle) {
                    return true;
                }
                state.posts.iter().any(|entry| {
                    let post = entry.value();
                    post.thread_id == thread.id
                        && !post.hidden
                        && post.body.to_lowercase().contains(&needle)
                })
            })
            .map(|thread| thread_view(&state, &thread, "", false))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
