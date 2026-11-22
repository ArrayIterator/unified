// Content Management module.
//
// Posts, categories, and pages with pagination, filtering, and search,
// slug generation with uniqueness guarantees, a draft -> published flow
// that emits the `content.published` hook, and automatic excerpt
// generation from the body text.

use crate::core::context::AppContext;
use crate::core::error::UnifiedError;
use crate::core::hooks::HookBus;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Publication status of a post or a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentStatus {
    Draft,
    Published,
}

impl Default for ContentStatus {
    fn default() -> Self {
        Self::Draft
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub id: Uuid,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub excerpt: String,
    pub status: ContentStatus,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: Uuid,
    pub title: String,
    pub slug: String,
    pub content: String,
    pub excerpt: String,
    pub status: ContentStatus,
    pub author_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePostInput {
    pub title: String,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostInput {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub category_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCategoryInput {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryInput {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePageInput {
    pub title: String,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePageInput {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
    pub author_id: Option<Uuid>,
    pub status: Option<ContentStatus>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct ContentState {
    pub posts: Arc<DashMap<Uuid, Post>>,
    pub categories: Arc<DashMap<Uuid, Category>>,
    pub pages: Arc<DashMap<Uuid, Page>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Turn a title into a URL slug: lowercase, ASCII alphanumerics kept,
/// every other run of characters collapsed to a single `-`.
pub fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = true; // suppresses a leading dash
    for ch in input.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// Append `-2`, `-3`, ... until the slug is free.
fn unique_slug(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Remove HTML tags from a string, keeping the text between them.
fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Build an excerpt: an explicit excerpt wins (trimmed, capped at 300
/// characters); otherwise the plain text of the body is truncated to 160
/// characters at the last word boundary and suffixed with an ellipsis.
pub fn make_excerpt(content: &str, provided: Option<&str>) -> String {
    if let Some(text) = provided {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.chars().take(300).collect();
        }
    }
    let plain: String = strip_html(content)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if plain.chars().count() <= 160 {
        return plain;
    }
    let cut: String = plain.chars().take(160).collect();
    match cut.rfind(' ') {
        Some(index) => format!("{}...", &cut[..index]),
        None => format!("{cut}..."),
    }
}

fn validate_title(title: &str) -> Result<String, UnifiedError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(UnifiedError::bad_request("title must not be empty"));
    }
    if title.chars().count() > 500 {
        return Err(UnifiedError::bad_request(
            "title must be at most 500 characters",
        ));
    }
    Ok(title.to_string())
}

fn validate_content(content: Option<String>) -> Result<String, UnifiedError> {
    let content = content.unwrap_or_default();
    if content.chars().count() > 1_000_000 {
        return Err(UnifiedError::bad_request(
            "content must be at most 1,000,000 characters",
        ));
    }
    Ok(content)
}

/// Resolve a caller-supplied slug (or derive one from the title) and make
/// it unique inside the collection described by `taken`.
fn resolve_slug(
    requested: Option<String>,
    title: &str,
    taken: &dyn Fn(&str) -> bool,
) -> Result<String, UnifiedError> {
    let base = match requested {
        Some(raw) => slugify(&raw),
        None => slugify(title),
    };
    if base.is_empty() {
        return Err(UnifiedError::bad_request(
            "slug must contain at least one letter or digit",
        ));
    }
    Ok(unique_slug(&base, taken))
}

/// Slice a sorted, filtered list into one page.
fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn sort_posts(mut items: Vec<Post>, params: &PaginationParams) -> Vec<Post> {
    let sort = params.allowed_sort(&["created_at", "updated_at", "title"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

fn sort_pages(mut items: Vec<Page>, params: &PaginationParams) -> Vec<Page> {
    let sort = params.allowed_sort(&["created_at", "updated_at", "title"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

fn sort_categories(mut items: Vec<Category>, params: &PaginationParams) -> Vec<Category> {
    let sort = params.allowed_sort(&["created_at", "name"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

// ---------------------------------------------------------------------------
// Post handlers
// ---------------------------------------------------------------------------

async fn list_posts(
    state: web::Data<ContentState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone();
    let status_filter = params.filters.get("status").cloned();
    let category_filter = params.filters.get("category_id").cloned();
    let author_filter = params.filters.get("author_id").cloned();
    let mut items: Vec<Post> = state
        .posts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|post| {
            if let Some(status) = &status_filter {
                let wanted = match status.as_str() {
                    "published" => ContentStatus::Published,
                    _ => ContentStatus::Draft,
                };
                if post.status != wanted {
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

async fn create_post(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<CreatePostInput>,
) -> HttpResponse {
    let input = input.into_inner();
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
            author_id: input.author_id,
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
                emit_published(&ctx, "post", &post.id, &post.title, &post.slug, &post.content);
            }
            api_json::success_response(post, 201)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn get_post(state: web::Data<ContentState>, id: web::Path<Uuid>) -> HttpResponse {
    match state.posts.get(&id.into_inner()) {
        Some(post) => api_json::success_response(post.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("post not found"), false),
    }
}

async fn update_post(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    input: web::Json<UpdatePostInput>,
) -> HttpResponse {
    let id = id.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<(Post, bool), UnifiedError> {
        let mut post = state
            .posts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("post not found"))?;
        let mut newly_published = false;
        if let Some(title) = &input.title {
            post.title = validate_title(title)?;
        }
        if let Some(content) = input.content {
            post.content = validate_content(Some(content))?;
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
        if let Some(status) = input.status {
            if status != post.status {
                if status == ContentStatus::Published {
                    post.published_at = Some(Utc::now());
                    newly_published = true;
                } else {
                    post.published_at = None;
                }
                post.status = status;
            }
        }
        post.updated_at = Utc::now();
        state.posts.insert(id, post.clone());
        Ok((post, newly_published))
    })();
    match outcome {
        Ok((post, newly_published)) => {
            if newly_published {
                emit_published(&ctx, "post", &post.id, &post.title, &post.slug, &post.content);
            }
            api_json::success_response(post, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn delete_post(state: web::Data<ContentState>, id: web::Path<Uuid>) -> HttpResponse {
    let id = id.into_inner();
    match state.posts.remove(&id) {
        Some((_, post)) => api_json::success_response(
            json!({ "id": post.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("post not found"), false),
    }
}

async fn publish_post(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
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
            emit_published(&ctx, "post", &post.id, &post.title, &post.slug, &post.content);
            api_json::success_response(post, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn unpublish_post(
    state: web::Data<ContentState>,
    id: web::Path<Uuid>,
) -> HttpResponse {
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
        Ok(post) => api_json::success_response(post, 200),
        Err(error) => api_json::error_response(&error, false),
    }
}

/// Emit the `content.published` hook for a newly published post or page.
fn emit_published(
    ctx: &AppContext,
    kind: &str,
    id: &Uuid,
    title: &str,
    slug: &str,
    content: &str,
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
        }),
    );
}

// ---------------------------------------------------------------------------
// Category handlers
// ---------------------------------------------------------------------------

async fn list_categories(
    state: web::Data<ContentState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn create_category(
    state: web::Data<ContentState>,
    input: web::Json<CreateCategoryInput>,
) -> HttpResponse {
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
        Err(error) => api_json::error_response(&error, false),
    }
}

async fn get_category(state: web::Data<ContentState>, id: web::Path<Uuid>) -> HttpResponse {
    match state.categories.get(&id.into_inner()) {
        Some(category) => api_json::success_response(category.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("category not found"), false),
    }
}

async fn update_category(
    state: web::Data<ContentState>,
    id: web::Path<Uuid>,
    input: web::Json<UpdateCategoryInput>,
) -> HttpResponse {
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
        Err(error) => api_json::error_response(&error, false),
    }
}

async fn delete_category(state: web::Data<ContentState>, id: web::Path<Uuid>) -> HttpResponse {
    let id = id.into_inner();
    if state
        .posts
        .iter()
        .any(|entry| entry.value().category_id == Some(id))
    {
        return api_json::error_response(
            &UnifiedError::conflict("category is still used by one or more posts"),
            false,
        );
    }
    match state.categories.remove(&id) {
        Some((_, category)) => api_json::success_response(
            json!({ "id": category.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("category not found"), false),
    }
}

// ---------------------------------------------------------------------------
// Page handlers
// ---------------------------------------------------------------------------

async fn list_pages(
    state: web::Data<ContentState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone();
    let status_filter = params.filters.get("status").cloned();
    let mut items: Vec<Page> = state
        .pages
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|page| {
            if let Some(status) = &status_filter {
                let wanted = match status.as_str() {
                    "published" => ContentStatus::Published,
                    _ => ContentStatus::Draft,
                };
                if page.status != wanted {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !matches_search(&page.title, needle)
                    && !matches_search(&page.content, needle)
                    && !matches_search(&page.slug, needle)
                {
                    return false;
                }
            }
            true
        })
        .collect();
    items = sort_pages(items, &params);
    api_json::success_response(paginate(items, &params).to_json(), 200)
}

async fn create_page(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<CreatePageInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let outcome = (|| -> Result<Page, UnifiedError> {
        let title = validate_title(&input.title)?;
        let content = validate_content(input.content)?;
        let slug = resolve_slug(input.slug, &title, &|candidate| {
            state
                .pages
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
        let page = Page {
            id: Uuid::now_v7(),
            excerpt: make_excerpt(&content, input.excerpt.as_deref()),
            title,
            slug,
            content,
            status,
            author_id: input.author_id,
            created_at: now,
            updated_at: now,
            published_at,
        };
        state.pages.insert(page.id, page.clone());
        Ok(page)
    })();
    match outcome {
        Ok(page) => {
            if page.status == ContentStatus::Published {
                emit_published(&ctx, "page", &page.id, &page.title, &page.slug, &page.content);
            }
            api_json::success_response(page, 201)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn get_page(state: web::Data<ContentState>, id: web::Path<Uuid>) -> HttpResponse {
    match state.pages.get(&id.into_inner()) {
        Some(page) => api_json::success_response(page.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("page not found"), false),
    }
}

async fn update_page(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    input: web::Json<UpdatePageInput>,
) -> HttpResponse {
    let id = id.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<(Page, bool), UnifiedError> {
        let mut page = state
            .pages
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("page not found"))?;
        let mut newly_published = false;
        if let Some(title) = &input.title {
            page.title = validate_title(title)?;
        }
        if let Some(content) = input.content {
            page.content = validate_content(Some(content))?;
        }
        if input.author_id.is_some() {
            page.author_id = input.author_id;
        }
        if let Some(slug) = input.slug {
            let base = slugify(&slug);
            if base.is_empty() {
                return Err(UnifiedError::bad_request(
                    "slug must contain at least one letter or digit",
                ));
            }
            page.slug = unique_slug(&base, &|candidate| {
                state.pages.iter().any(|entry| {
                    entry.value().slug == candidate && entry.key() != &id
                })
            });
        }
        if let Some(excerpt) = input.excerpt.as_deref() {
            page.excerpt = make_excerpt(&page.content, Some(excerpt));
        } else if input.content.is_some() {
            page.excerpt = make_excerpt(&page.content, None);
        }
        if let Some(status) = input.status {
            if status != page.status {
                if status == ContentStatus::Published {
                    page.published_at = Some(Utc::now());
                    newly_published = true;
                } else {
                    page.published_at = None;
                }
                page.status = status;
            }
        }
        page.updated_at = Utc::now();
        state.pages.insert(id, page.clone());
        Ok((page, newly_published))
    })();
    match outcome {
        Ok((page, newly_published)) => {
            if newly_published {
                emit_published(&ctx, "page", &page.id, &page.title, &page.slug, &page.content);
            }
            api_json::success_response(page, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn delete_page(state: web::Data<ContentState>, id: web::Path<Uuid>) -> HttpResponse {
    let id = id.into_inner();
    match state.pages.remove(&id) {
        Some((_, page)) => api_json::success_response(
            json!({ "id": page.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("page not found"), false),
    }
}

async fn publish_page(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let id = id.into_inner();
    let outcome = (|| -> Result<Page, UnifiedError> {
        let mut page = state
            .pages
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("page not found"))?;
        if page.status == ContentStatus::Published {
            return Err(UnifiedError::conflict("page is already published"));
        }
        page.status = ContentStatus::Published;
        page.published_at = Some(Utc::now());
        page.updated_at = Utc::now();
        state.pages.insert(id, page.clone());
        Ok(page)
    })();
    match outcome {
        Ok(page) => {
            emit_published(&ctx, "page", &page.id, &page.title, &page.slug, &page.content);
            api_json::success_response(page, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn unpublish_page(
    state: web::Data<ContentState>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let id = id.into_inner();
    let outcome = (|| -> Result<Page, UnifiedError> {
        let mut page = state
            .pages
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("page not found"))?;
        if page.status == ContentStatus::Draft {
            return Err(UnifiedError::conflict("page is already a draft"));
        }
        page.status = ContentStatus::Draft;
        page.published_at = None;
        page.updated_at = Utc::now();
        state.pages.insert(id, page.clone());
        Ok(page)
    })();
    match outcome {
        Ok(page) => api_json::success_response(page, 200),
        Err(error) => api_json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `content.ai.<capability>_requested` hooks, and run through the AI
// module's gated runner. Drafts are returned inline and are never
// published by this module.
// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. Both are non-PII: inputs are redacted before they
/// reach the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft",
        module_id: "content",
        name: "Draft content",
        description: "Draft post or page text from a title and a short brief. The output is returned inline; it is never published by this module.",
        input_fields: &["title", "brief"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "summarize",
        module_id: "content",
        name: "Summarize text",
        description: "Summarize caller-supplied text. Related published posts and pages ground the call as style context; the text itself comes from the caller.",
        input_fields: &["text"],
        allows_pii: false,
        use_case: "complete",
    },
];

/// Related published posts and pages for one grounding query:
/// title matches weigh most, body occurrences cap at five per
/// token; at most three items, published only.
fn related_content(state: &ContentState, query: &str) -> Vec<Value> {
    let tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(f64, Value)> = Vec::new();
    for entry in state.posts.iter() {
        let post = entry.value();
        if post.status != ContentStatus::Published {
            continue;
        }
        let title = post.title.to_lowercase();
        let body = post.content.to_lowercase();
        let mut score = 0f64;
        for token in &tokens {
            if title.contains(token.as_str()) {
                score += 3.0;
            }
            score += (body.matches(token.as_str()).count() as f64).min(5.0);
        }
        if score > 0.0 {
            scored.push((
                score,
                json!({
                    "id": post.id.to_string(),
                    "title": post.title,
                    "excerpt": post.excerpt,
                    "url": format!("/api/v1/content/posts/{}", post.id),
                }),
            ));
        }
    }
    for entry in state.pages.iter() {
        let page = entry.value();
        if page.status != ContentStatus::Published {
            continue;
        }
        let title = page.title.to_lowercase();
        let body = page.content.to_lowercase();
        let mut score = 0f64;
        for token in &tokens {
            if title.contains(token.as_str()) {
                score += 3.0;
            }
            score += (body.matches(token.as_str()).count() as f64).min(5.0);
        }
        if score > 0.0 {
            scored.push((
                score,
                json!({
                    "id": page.id.to_string(),
                    "title": page.title,
                    "excerpt": page.excerpt,
                    "url": format!("/api/v1/content/pages/{}", page.id),
                }),
            ));
        }
    }
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.into_iter().take(3).map(|(_, value)| value).collect()
}

/// Run one registered content AI capability:
/// `POST /api/v1/content/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use`; delegates to the AI
/// module's gated runner.
async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &caller.role, ai::PERM_AI_USE) {
        return api_json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &caller);
    match ai::run_ai_capability(
        &ctx,
        &caller,
        company_id,
        "content",
        &capability,
        body.into_inner().input,
    )
    .await
    {
        Ok(data) => api_json::success_response(data, 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ContentModule {
    state: ContentState,
}

impl ContentModule {
    pub fn new() -> Self {
        Self {
            state: ContentState::default(),
        }
    }
}

impl Default for ContentModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ContentModule {
    fn id(&self) -> &'static str {
        "content"
    }

    fn name(&self) -> &'static str {
        "Content Management"
    }

    fn description(&self) -> &'static str {
        "Posts, categories, and pages with slug handling, publishing, and excerpts."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("content", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Ground AI capability calls with related published posts
        // and pages (style context only; the caller supplies the
        // text to draft or summarize). Tenant scoping: this store
        // is in-memory with no tenant partition yet, so there is
        // nothing to filter on; when batch 17 persistence lands,
        // this handler must return only the calling tenant's
        // content, keyed by the caller company id in the payload.
        for event in [
            "content.ai.draft_requested",
            "content.ai.summarize_requested",
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "content",
                Arc::new(move |ctx| {
                    let input = ctx.get("input").cloned().unwrap_or(Value::Null);
                    let query = ["title", "brief", "text"]
                        .iter()
                        .filter_map(|field| {
                            input.get(*field).and_then(|value| value.as_str())
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let related = related_content(&state, &query);
                    ctx.add_result(json!({ "related": related }));
                }),
            );
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/content/posts", owner: "content", description: "List posts" },
            RouteInfo { method: "POST", path: "/api/v1/content/posts", owner: "content", description: "Create post" },
            RouteInfo { method: "GET", path: "/api/v1/content/posts/{id}", owner: "content", description: "Get post" },
            RouteInfo { method: "PUT", path: "/api/v1/content/posts/{id}", owner: "content", description: "Update post" },
            RouteInfo { method: "DELETE", path: "/api/v1/content/posts/{id}", owner: "content", description: "Delete post" },
            RouteInfo { method: "POST", path: "/api/v1/content/posts/{id}/publish", owner: "content", description: "Publish post" },
            RouteInfo { method: "POST", path: "/api/v1/content/posts/{id}/unpublish", owner: "content", description: "Unpublish post" },
            RouteInfo { method: "GET", path: "/api/v1/content/categories", owner: "content", description: "List categories" },
            RouteInfo { method: "POST", path: "/api/v1/content/categories", owner: "content", description: "Create category" },
            RouteInfo { method: "GET", path: "/api/v1/content/categories/{id}", owner: "content", description: "Get category" },
            RouteInfo { method: "PUT", path: "/api/v1/content/categories/{id}", owner: "content", description: "Update category" },
            RouteInfo { method: "DELETE", path: "/api/v1/content/categories/{id}", owner: "content", description: "Delete category" },
            RouteInfo { method: "GET", path: "/api/v1/content/pages", owner: "content", description: "List pages" },
            RouteInfo { method: "POST", path: "/api/v1/content/pages", owner: "content", description: "Create page" },
            RouteInfo { method: "GET", path: "/api/v1/content/pages/{id}", owner: "content", description: "Get page" },
            RouteInfo { method: "PUT", path: "/api/v1/content/pages/{id}", owner: "content", description: "Update page" },
            RouteInfo { method: "DELETE", path: "/api/v1/content/pages/{id}", owner: "content", description: "Delete page" },
            RouteInfo { method: "POST", path: "/api/v1/content/pages/{id}/publish", owner: "content", description: "Publish page" },
            RouteInfo { method: "POST", path: "/api/v1/content/pages/{id}/unpublish", owner: "content", description: "Unpublish page" },
            RouteInfo { method: "POST", path: "/api/v1/content/ai/{capability}", owner: "content", description: "Run one registered content AI capability (draft, summarize)" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/content")
                .app_data(state)
                .route("/posts", web::get().to(list_posts))
                .route("/posts", web::post().to(create_post))
                .route("/posts/{id}", web::get().to(get_post))
                .route("/posts/{id}", web::put().to(update_post))
                .route("/posts/{id}", web::delete().to(delete_post))
                .route("/posts/{id}/publish", web::post().to(publish_post))
                .route("/posts/{id}/unpublish", web::post().to(unpublish_post))
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::put().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/pages", web::get().to(list_pages))
                .route("/pages", web::post().to(create_page))
                .route("/pages/{id}", web::get().to(get_page))
                .route("/pages/{id}", web::put().to(update_page))
                .route("/pages/{id}", web::delete().to(delete_page))
                .route("/pages/{id}/publish", web::post().to(publish_page))
                .route("/pages/{id}/unpublish", web::post().to(unpublish_page))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ContentModule::new()));
}
