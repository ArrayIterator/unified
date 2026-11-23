//! HTTP handlers for categories and topics.

use super::helpers::{
    caller, caller_company, can_manage, check_category_visible, check_topic_rate_limit, created,
    err, load_category, load_topic, ok, paginate, record_moderation, require_permission,
    require_tenant, require_text, slugify, topic_view, valid_slug,
};
use super::state::CommunityState;
use super::types::{
    CategoryVisibility, CommunityCategory, CommunityTopic, CreateCategoryRequest,
    CreateTopicRequest, MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_TAG_LEN, MAX_TAGS, MAX_TITLE_LEN,
    PERM_COMMUNITY_MANAGE, PERM_COMMUNITY_VIEW, UpdateCategoryRequest, UpdateTopicRequest,
};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Category handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_categories(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let visibility_filter = params.filters.get("visibility").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<CommunityCategory> = state
            .categories
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|category| {
                if category.visibility == CategoryVisibility::StaffOnly && !is_manager {
                    return false;
                }
                if let Some(visibility) = &visibility_filter {
                    if category.visibility.as_str() != visibility {
                        return false;
                    }
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

pub(crate) async fn create_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&name),
        };
        if state.categories.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
        }
        let now = Utc::now();
        let category = CommunityCategory {
            id: Uuid::now_v7(),
            name,
            description,
            slug,
            sort_order: input.sort_order.unwrap_or(0),
            visibility: input.visibility.unwrap_or_default(),
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        state.emit(
            "community.category.created",
            json!({
                "category_id": category.id.to_string(),
                "slug": category.slug,
                "name": category.name,
                "visibility": category.visibility.as_str(),
            }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => created(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let category = load_category(&state, &path.into_inner())?;
        check_category_visible(&state, &ctx, &caller.actor, &caller.role, &category)?;
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut category = load_category(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            category.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            category.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if state
                .categories
                .iter()
                .any(|entry| entry.value().slug == slug && entry.value().id != category.id)
            {
                return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
            }
            category.slug = slug;
        }
        if let Some(sort_order) = input.sort_order {
            category.sort_order = sort_order;
        }
        if let Some(visibility) = input.visibility {
            category.visibility = visibility;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        state.emit(
            "community.category.updated",
            json!({ "category_id": category.id.to_string(), "slug": category.slug }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let category = load_category(&state, &path.into_inner())?;
        if state
            .topics
            .iter()
            .any(|entry| entry.value().category_id == category.id)
        {
            return Err(UnifiedError::conflict(
                "A category with topics cannot be deleted; move or delete its topics first",
            ));
        }
        let id = category.id;
        state.categories.remove(&id);
        state.emit(
            "community.category.deleted",
            json!({ "category_id": id.to_string(), "slug": category.slug }),
        );
        Ok(json!({ "deleted": true, "category_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Topic handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_topics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let category_filter = params
            .filters
            .get("category_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let tag_filter = params.filters.get("tag").cloned();
        let pinned_filter = params
            .filters
            .get("pinned")
            .and_then(|raw| raw.parse::<bool>().ok());
        let author_filter = params.filters.get("author_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .topics
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|topic| {
                let Some(category) = state.categories.get(&topic.category_id) else {
                    return false;
                };
                if category.visibility == CategoryVisibility::StaffOnly && !is_manager {
                    return false;
                }
                if let Some(category_id) = category_filter {
                    if topic.category_id != category_id {
                        return false;
                    }
                }
                if let Some(tag) = &tag_filter {
                    if !topic.tags.iter().any(|entry| entry == tag) {
                        return false;
                    }
                }
                if let Some(pinned) = pinned_filter {
                    if topic.pinned != pinned {
                        return false;
                    }
                }
                if let Some(author) = &author_filter {
                    if topic.author_id != *author {
                        return false;
                    }
                }
                if !search.is_empty() && !topic.title.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .map(|topic| topic_view(&state, &topic, &caller.actor, is_manager))
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        // Pinned topics sort first within the same page order.
        items.sort_by_key(|item| {
            !item
                .get("pinned")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<CreateTopicRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let input = body.into_inner();
        let category = state
            .categories
            .get(&input.category_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Category not found: {}", input.category_id))
            })?;
        check_category_visible(&state, &ctx, &caller.actor, &caller.role, &category)?;
        check_topic_rate_limit(&state, &caller.actor)?;
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        if input.tags.len() > MAX_TAGS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_TAGS} tags are allowed"
            )));
        }
        let mut tags: Vec<String> = Vec::new();
        for raw in &input.tags {
            tags.push(require_text(raw, "tag", MAX_TAG_LEN)?);
        }
        let now = Utc::now();
        let topic = CommunityTopic {
            id: Uuid::now_v7(),
            category_id: category.id,
            title,
            author_id: caller.actor.clone(),
            tags,
            pinned: false,
            locked: false,
            created_at: now,
            updated_at: now,
        };
        state.topics.insert(topic.id, topic.clone());
        state.emit(
            "community.topic.created",
            json!({
                "topic_id": topic.id.to_string(),
                "category_id": topic.category_id.to_string(),
                "title": topic.title,
                "author_id": topic.author_id,
            }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => created(topic),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let topic = load_topic(&state, &path.into_inner())?;
        let category = state
            .categories
            .get(&topic.category_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Category not found: {}", topic.category_id))
            })?;
        check_category_visible(&state, &ctx, &caller.actor, &caller.role, &category)?;
        Ok(topic_view(&state, &topic, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTopicRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut topic = load_topic(&state, &path.into_inner())?;
        if topic.author_id != caller.actor && !is_manager {
            return Err(UnifiedError::forbidden(
                "Only the topic author or a moderator can update this topic",
            ));
        }
        let input = body.into_inner();
        if let Some(title) = &input.title {
            topic.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(category_id) = input.category_id {
            let category = state
                .categories
                .get(&category_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {category_id}")))?;
            check_category_visible(&state, &ctx, &caller.actor, &caller.role, &category)?;
            topic.category_id = category_id;
        }
        if let Some(tags) = &input.tags {
            if tags.len() > MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_TAGS} tags are allowed"
                )));
            }
            let mut checked: Vec<String> = Vec::new();
            for raw in tags {
                checked.push(require_text(raw, "tag", MAX_TAG_LEN)?);
            }
            topic.tags = checked;
        }
        topic.updated_at = Utc::now();
        state.topics.insert(topic.id, topic.clone());
        state.emit(
            "community.topic.updated",
            json!({ "topic_id": topic.id.to_string(), "title": topic.title }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => ok(topic),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let topic = load_topic(&state, &path.into_inner())?;
        if topic.author_id != caller.actor && !is_manager {
            return Err(UnifiedError::forbidden(
                "Only the topic author or a moderator can delete this topic",
            ));
        }
        let id = topic.id;
        state.topics.remove(&id);
        let stale_posts: Vec<Uuid> = state
            .posts
            .iter()
            .filter(|entry| entry.value().topic_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in &stale_posts {
            state.posts.remove(key);
        }
        let stale_votes: Vec<Uuid> = state
            .votes
            .iter()
            .filter(|entry| {
                stale_posts.contains(&entry.value().post_id)
            })
            .map(|entry| *entry.key())
            .collect();
        for key in stale_votes {
            state.votes.remove(&key);
        }
        let stale_flags: Vec<Uuid> = state
            .flags
            .iter()
            .filter(|entry| {
                stale_posts.contains(&entry.value().post_id)
            })
            .map(|entry| *entry.key())
            .collect();
        for key in stale_flags {
            state.flags.remove(&key);
        }
        state.emit(
            "community.topic.deleted",
            json!({ "topic_id": id.to_string(), "title": topic.title }),
        );
        Ok(json!({ "deleted": true, "topic_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn pin_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut topic = load_topic(&state, &path.into_inner())?;
        topic.pinned = !topic.pinned;
        topic.updated_at = Utc::now();
        state.topics.insert(topic.id, topic.clone());
        record_moderation(
            &state,
            "topic",
            topic.id,
            if topic.pinned { "pin" } else { "unpin" },
            &caller.actor,
            None,
        );
        state.emit(
            "community.topic.pinned",
            json!({ "topic_id": topic.id.to_string(), "pinned": topic.pinned }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => ok(topic),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn lock_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut topic = load_topic(&state, &path.into_inner())?;
        topic.locked = !topic.locked;
        topic.updated_at = Utc::now();
        state.topics.insert(topic.id, topic.clone());
        record_moderation(
            &state,
            "topic",
            topic.id,
            if topic.locked { "lock" } else { "unlock" },
            &caller.actor,
            None,
        );
        state.emit(
            "community.topic.locked",
            json!({ "topic_id": topic.id.to_string(), "locked": topic.locked }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => ok(topic),
        Err(error) => err(error, debug),
    }
}
