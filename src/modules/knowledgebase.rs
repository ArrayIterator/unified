// Knowledgebase module.
//
// Articles with full version history, categories, token-based full-text
// search, and ticket links. The search index is also exposed to other
// modules through the `knowledgebase.search` hook event so the AI Support
// assistant can ground its answers in real articles instead of inventing
// them.
//
// Routes live under `/api/v1/knowledgebase`:
//   - `GET/POST  /articles` — list (paginated, filterable) / create.
//   - `GET/PATCH/DELETE /articles/{id}` — read / update / delete.
//   - `GET /articles/{id}/versions` and `GET /articles/{id}/versions/{v}`.
//   - `GET/POST  /categories`, `GET/PATCH/DELETE /categories/{id}`.
//   - `GET /search?q=...` — ranked full-text search.
//   - `POST /articles/{id}/link-ticket`, `GET/DELETE /articles/{id}/tickets`.
//
// Hook contract for `knowledgebase.search` (emitted by AI Support):
//   - Input payload: `{ "query": "<text>", "limit": <number> }`.
//   - This handler appends one result: `{ "articles": [...] }` where each
//     article is `{ "id", "title", "excerpt", "score", "url" }` and only
//     published articles are returned.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_BODY_LEN: usize = 50_000;
const MAX_TAG_LEN: usize = 50;
const MAX_TAGS: usize = 20;
const MAX_NAME_LEN: usize = 120;
const MAX_TICKET_ID_LEN: usize = 100;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Publication status of an article.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArticleStatus {
    Draft,
    Published,
    Archived,
}

impl Default for ArticleStatus {
    fn default() -> Self {
        Self::Draft
    }
}

/// A knowledgebase article. `version` is the current version number; every
/// update stores a new [`ArticleVersion`] row and bumps this number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Article {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub category_id: Option<Uuid>,
    pub status: ArticleStatus,
    pub tags: Vec<String>,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One immutable snapshot of an article, created on every update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleVersion {
    pub article_id: Uuid,
    pub version: u32,
    pub title: String,
    pub body: String,
    pub status: ArticleStatus,
    pub created_at: DateTime<Utc>,
}

/// A category grouping articles. Categories may be nested via `parent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A link between an article and a support ticket (owned by the tickets
/// module; only the ticket id is stored here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketLink {
    pub article_id: Uuid,
    pub ticket_id: String,
    pub linked_at: DateTime<Utc>,
}

/// One ranked search hit.
#[derive(Debug, Clone, Serialize)]
pub struct ArticleSearchHit {
    pub id: Uuid,
    pub title: String,
    pub excerpt: String,
    pub score: f64,
    pub category_id: Option<Uuid>,
    pub status: ArticleStatus,
    pub tags: Vec<String>,
    pub url: String,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateArticleRequest {
    pub title: String,
    pub body: String,
    pub category_id: Option<Uuid>,
    pub status: Option<ArticleStatus>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateArticleRequest {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `null` clears the category, a value moves the article.
    pub category_id: Option<Option<Uuid>>,
    pub status: Option<ArticleStatus>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<Option<String>>,
    pub parent_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
pub struct LinkTicketRequest {
    pub ticket_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub status: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the knowledgebase. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by simply skipping event emission.
#[derive(Clone, Default)]
pub struct KnowledgebaseState {
    pub articles: Arc<DashMap<Uuid, Article>>,
    pub versions: Arc<DashMap<Uuid, Vec<ArticleVersion>>>,
    pub categories: Arc<DashMap<Uuid, Category>>,
    pub ticket_links: Arc<DashMap<Uuid, Vec<TicketLink>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl KnowledgebaseState {
    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.hooks.read().expect("hooks lock poisoned").clone() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn actor_id(req: &HttpRequest) -> UnifiedResult<String> {
    req.headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

fn clean_tags(tags: Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags.unwrap_or_default() {
        let cleaned = require_text(&tag, "tag", MAX_TAG_LEN)?.to_lowercase();
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    if out.len() > MAX_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_TAGS} tags are allowed"
        )));
    }
    Ok(out)
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
}

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

/// Split text into lowercase alphanumeric tokens.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

/// Build a short excerpt around the first query token found in the body.
fn excerpt(body: &str, tokens: &[String]) -> String {
    let chars: Vec<char> = body.chars().collect();
    let lower: String = body.to_lowercase();
    let mut start = 0usize;
    for token in tokens {
        if let Some(pos) = lower.find(token.as_str()) {
            let char_pos = lower[..pos].chars().count();
            start = char_pos.saturating_sub(80);
            break;
        }
    }
    let end = (start + 240).min(chars.len());
    let mut text: String = chars[start..end].iter().collect();
    if start > 0 {
        text = format!("...{text}");
    }
    if end < chars.len() {
        text.push_str("...");
    }
    text.trim().to_string()
}

/// Token-based ranked search over title, tags and body.
///
/// Scoring: a token in the title is worth 3 points, in a tag 2 points, and
/// each occurrence in the body (capped at 5 per token) 1 point. Results are
/// sorted by score descending, then by most recently updated.
pub fn search_articles(
    articles: &DashMap<Uuid, Article>,
    query: &str,
    only_published: bool,
    limit: usize,
) -> Vec<ArticleSearchHit> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(f64, Article)> = Vec::new();
    for entry in articles.iter() {
        let article = entry.value();
        if only_published && article.status != ArticleStatus::Published {
            continue;
        }
        let title = article.title.to_lowercase();
        let body = article.body.to_lowercase();
        let tags: Vec<String> = article.tags.iter().map(|tag| tag.to_lowercase()).collect();
        let mut score = 0f64;
        for token in &tokens {
            if title.contains(token.as_str()) {
                score += 3.0;
            }
            if tags.iter().any(|tag| tag == token || tag.contains(token.as_str())) {
                score += 2.0;
            }
            let occurrences = body.matches(token.as_str()).count() as f64;
            score += occurrences.min(5.0);
        }
        if score > 0.0 {
            scored.push((score, article.clone()));
        }
    }
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.1.updated_at.cmp(&a.1.updated_at))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(score, article)| ArticleSearchHit {
            id: article.id,
            title: article.title.clone(),
            excerpt: excerpt(&article.body, &tokens),
            score,
            category_id: article.category_id,
            status: article.status,
            tags: article.tags.clone(),
            url: format!("/api/v1/knowledgebase/articles/{}", article.id),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct KnowledgebaseModule {
    state: KnowledgebaseState,
}

impl KnowledgebaseModule {
    pub fn new() -> Self {
        Self {
            state: KnowledgebaseState::default(),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(KnowledgebaseModule::new()));
}

impl Module for KnowledgebaseModule {
    fn id(&self) -> &'static str {
        "knowledgebase"
    }

    fn name(&self) -> &'static str {
        "Knowledgebase"
    }

    fn description(&self) -> &'static str {
        "Versioned knowledgebase articles, categories, ranked full-text search, and ticket links."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["tickets"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Answer `knowledgebase.search` for AI Support (and any other
        // module): grounded answers must come from real articles.
        let state = self.state.clone();
        bus.register(
            "knowledgebase.search",
            "knowledgebase",
            Arc::new(move |ctx| {
                let query = ctx
                    .get("query")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let limit = ctx
                    .get("limit")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(5) as usize;
                if query.trim().is_empty() {
                    ctx.add_result(json!({ "articles": [] }));
                    return;
                }
                let hits = search_articles(&state.articles, &query, true, limit);
                let articles: Vec<Value> = hits
                    .iter()
                    .map(|hit| {
                        json!({
                            "id": hit.id.to_string(),
                            "title": hit.title,
                            "excerpt": hit.excerpt,
                            "score": hit.score,
                            "url": hit.url,
                        })
                    })
                    .collect();
                ctx.add_result(json!({ "articles": articles }));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles", owner: "knowledgebase", description: "List articles" },
            RouteInfo { method: "POST", path: "/api/v1/knowledgebase/articles", owner: "knowledgebase", description: "Create article" },
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}", owner: "knowledgebase", description: "Get article" },
            RouteInfo { method: "PATCH", path: "/api/v1/knowledgebase/articles/{id}", owner: "knowledgebase", description: "Update article" },
            RouteInfo { method: "DELETE", path: "/api/v1/knowledgebase/articles/{id}", owner: "knowledgebase", description: "Delete article" },
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}/versions", owner: "knowledgebase", description: "List versions" },
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}/versions/{version}", owner: "knowledgebase", description: "Get version" },
            RouteInfo { method: "POST", path: "/api/v1/knowledgebase/articles/{id}/link-ticket", owner: "knowledgebase", description: "Link ticket" },
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}/tickets", owner: "knowledgebase", description: "List article tickets" },
            RouteInfo { method: "DELETE", path: "/api/v1/knowledgebase/articles/{id}/tickets/{ticket_id}", owner: "knowledgebase", description: "Unlink ticket" },
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/categories", owner: "knowledgebase", description: "List categories" },
            RouteInfo { method: "POST", path: "/api/v1/knowledgebase/categories", owner: "knowledgebase", description: "Create category" },
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/categories/{id}", owner: "knowledgebase", description: "Get category" },
            RouteInfo { method: "PATCH", path: "/api/v1/knowledgebase/categories/{id}", owner: "knowledgebase", description: "Update category" },
            RouteInfo { method: "DELETE", path: "/api/v1/knowledgebase/categories/{id}", owner: "knowledgebase", description: "Delete category" },
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/search", owner: "knowledgebase", description: "Search endpoint" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/knowledgebase")
                .app_data(state)
                .route("/articles", web::get().to(list_articles))
                .route("/articles", web::post().to(create_article))
                .route("/articles/{id}", web::get().to(get_article))
                .route("/articles/{id}", web::patch().to(update_article))
                .route("/articles/{id}", web::delete().to(delete_article))
                .route(
                    "/articles/{id}/versions",
                    web::get().to(list_versions),
                )
                .route(
                    "/articles/{id}/versions/{version}",
                    web::get().to(get_version),
                )
                .route(
                    "/articles/{id}/link-ticket",
                    web::post().to(link_ticket),
                )
                .route(
                    "/articles/{id}/tickets",
                    web::get().to(list_article_tickets),
                )
                .route(
                    "/articles/{id}/tickets/{ticket_id}",
                    web::delete().to(unlink_ticket),
                )
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::patch().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/search", web::get().to(search_endpoint)),
        );
    }
}

// ---------------------------------------------------------------------------
// Article handlers
// ---------------------------------------------------------------------------

async fn create_article(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    body: web::Json<CreateArticleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Article> = (|| {
        let _actor = actor_id(&req)?;
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let content = require_text(&body.body, "body", MAX_BODY_LEN)?;
        let tags = clean_tags(body.tags.clone())?;
        if let Some(category_id) = body.category_id {
            if !state.categories.contains_key(&category_id) {
                return Err(UnifiedError::not_found(format!(
                    "Category not found: {category_id}"
                )));
            }
        }
        let now = Utc::now();
        let article = Article {
            id: Uuid::now_v7(),
            title,
            body: content,
            category_id: body.category_id,
            status: body.status.unwrap_or_default(),
            tags,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        state.versions.insert(
            article.id,
            vec![ArticleVersion {
                article_id: article.id,
                version: 1,
                title: article.title.clone(),
                body: article.body.clone(),
                status: article.status,
                created_at: now,
            }],
        );
        state.articles.insert(article.id, article.clone());
        state.emit(
            "knowledgebase.article.created",
            json!({ "article_id": article.id.to_string(), "title": article.title.clone() }),
        );
        Ok(article)
    })();
    match result {
        Ok(article) => created(article),
        Err(error) => err(error),
    }
}

async fn list_articles(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Article>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let category_filter = params
            .filters
            .get("category_id")
            .map(|raw| parse_uuid(raw, "category"))
            .transpose()?;
        let tokens = params
            .search
            .as_ref()
            .map(|search| tokenize(search))
            .unwrap_or_default();
        let mut items: Vec<Article> = state
            .articles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|article| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "draft" => ArticleStatus::Draft,
                        "published" => ArticleStatus::Published,
                        "archived" => ArticleStatus::Archived,
                        _ => return false,
                    };
                    if article.status != expected {
                        return false;
                    }
                }
                if let Some(category_id) = category_filter {
                    if article.category_id != Some(category_id) {
                        return false;
                    }
                }
                if !tokens.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        article.title.to_lowercase(),
                        article.body.to_lowercase(),
                        article.tags.join(" ")
                    );
                    if !tokens.iter().any(|token| haystack.contains(token.as_str())) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "created_at", "updated_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn get_article(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Article> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        state
            .articles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Article not found: {id}")))
    })();
    match result {
        Ok(article) => ok(article),
        Err(error) => err(error),
    }
}

async fn update_article(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateArticleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Article> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        let mut article = state
            .articles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Article not found: {id}")))?;
        if let Some(title) = &body.title {
            article.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(content) = &body.body {
            article.body = require_text(content, "body", MAX_BODY_LEN)?;
        }
        if let Some(category_id) = body.category_id {
            if let Some(category_id) = category_id {
                if !state.categories.contains_key(&category_id) {
                    return Err(UnifiedError::not_found(format!(
                        "Category not found: {category_id}"
                    )));
                }
            }
            article.category_id = category_id;
        }
        if let Some(status) = body.status {
            article.status = status;
        }
        if let Some(tags) = &body.tags {
            article.tags = clean_tags(Some(tags.clone()))?;
        }
        // Every update stores a new version row.
        article.version += 1;
        article.updated_at = Utc::now();
        let version = ArticleVersion {
            article_id: article.id,
            version: article.version,
            title: article.title.clone(),
            body: article.body.clone(),
            status: article.status,
            created_at: article.updated_at,
        };
        match state.versions.get_mut(&article.id) {
            Some(mut versions) => versions.push(version),
            None => {
                state.versions.insert(article.id, vec![version]);
            }
        }
        state.articles.insert(article.id, article.clone());
        state.emit(
            "knowledgebase.article.updated",
            json!({
                "article_id": article.id.to_string(),
                "version": article.version,
            }),
        );
        Ok(article)
    })();
    match result {
        Ok(article) => ok(article),
        Err(error) => err(error),
    }
}

async fn delete_article(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        if state.articles.remove(&id).is_none() {
            return Err(UnifiedError::not_found(format!("Article not found: {id}")));
        }
        state.versions.remove(&id);
        state.ticket_links.remove(&id);
        state.emit(
            "knowledgebase.article.deleted",
            json!({ "article_id": id.to_string() }),
        );
        Ok(json!({ "deleted": true, "article_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

async fn list_versions(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ArticleVersion>> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        if !state.articles.contains_key(&id) {
            return Err(UnifiedError::not_found(format!("Article not found: {id}")));
        }
        let params = query.into_inner();
        let mut versions: Vec<ArticleVersion> = state
            .versions
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        versions.sort_by(|a, b| b.version.cmp(&a.version));
        Ok(paginate(versions, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn get_version(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<ArticleVersion> = (|| {
        let _actor = actor_id(&req)?;
        let (raw_id, raw_version) = path.into_inner();
        let id = parse_uuid(&raw_id, "article")?;
        let version: u32 = raw_version
            .parse()
            .map_err(|_| UnifiedError::bad_request(format!("Invalid version: {raw_version}")))?;
        if !state.articles.contains_key(&id) {
            return Err(UnifiedError::not_found(format!("Article not found: {id}")));
        }
        state
            .versions
            .get(&id)
            .and_then(|entry| {
                entry
                    .value()
                    .iter()
                    .find(|row| row.version == version)
                    .cloned()
            })
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Version {version} not found for article {id}"))
            })
    })();
    match result {
        Ok(version) => ok(version),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Search handler
// ---------------------------------------------------------------------------

async fn search_endpoint(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    query: web::Query<SearchQuery>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ArticleSearchHit>> = (|| {
        let _actor = actor_id(&req)?;
        let query = query.into_inner();
        let text = query.q.clone().unwrap_or_default();
        if text.trim().is_empty() {
            return Err(UnifiedError::bad_request("Query parameter q must not be empty"));
        }
        let status_filter = match query.status.as_deref() {
            None => Some(ArticleStatus::Published),
            Some("published") => Some(ArticleStatus::Published),
            Some("draft") => Some(ArticleStatus::Draft),
            Some("archived") => Some(ArticleStatus::Archived),
            Some("all") => None,
            Some(other) => {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid status filter: {other}"
                )));
            }
        };
        let mut hits = search_articles(&state.articles, &text, false, usize::MAX);
        if let Some(status) = status_filter {
            hits.retain(|hit| hit.status == status);
        }
        let params = PaginationParams {
            page: query.page,
            per_page: query.per_page,
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        };
        Ok(paginate(hits, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Ticket link handlers
// ---------------------------------------------------------------------------

async fn link_ticket(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkTicketRequest>,
) -> HttpResponse {
    let result: UnifiedResult<TicketLink> = (|| {
        let actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        if !state.articles.contains_key(&id) {
            return Err(UnifiedError::not_found(format!("Article not found: {id}")));
        }
        let ticket_id = require_text(&body.ticket_id, "ticket_id", MAX_TICKET_ID_LEN)?;
        let link = TicketLink {
            article_id: id,
            ticket_id: ticket_id.clone(),
            linked_at: Utc::now(),
        };
        match state.ticket_links.get_mut(&id) {
            Some(mut links) => {
                if links.iter().any(|row| row.ticket_id == ticket_id) {
                    return Err(UnifiedError::conflict(format!(
                        "Article {id} is already linked to ticket {ticket_id}"
                    )));
                }
                links.push(link.clone());
            }
            None => {
                state.ticket_links.insert(id, vec![link.clone()]);
            }
        }
        state.emit(
            "knowledgebase.article.ticket_linked",
            json!({
                "article_id": id.to_string(),
                "ticket_id": ticket_id,
                "linked_by": actor,
            }),
        );
        Ok(link)
    })();
    match result {
        Ok(link) => created(link),
        Err(error) => err(error),
    }
}

async fn list_article_tickets(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<TicketLink>> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        if !state.articles.contains_key(&id) {
            return Err(UnifiedError::not_found(format!("Article not found: {id}")));
        }
        let params = query.into_inner();
        let mut links: Vec<TicketLink> = state
            .ticket_links
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        links.sort_by(|a, b| b.linked_at.cmp(&a.linked_at));
        Ok(paginate(links, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn unlink_ticket(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _actor = actor_id(&req)?;
        let (raw_id, ticket_id) = path.into_inner();
        let id = parse_uuid(&raw_id, "article")?;
        if !state.articles.contains_key(&id) {
            return Err(UnifiedError::not_found(format!("Article not found: {id}")));
        }
        let mut removed = false;
        if let Some(mut links) = state.ticket_links.get_mut(&id) {
            let before = links.len();
            links.retain(|row| row.ticket_id != ticket_id);
            removed = links.len() < before;
        }
        if !removed {
            return Err(UnifiedError::not_found(format!(
                "Ticket {ticket_id} is not linked to article {id}"
            )));
        }
        Ok(json!({ "deleted": true, "article_id": id.to_string(), "ticket_id": ticket_id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Category handlers
// ---------------------------------------------------------------------------

async fn create_category(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Category> = (|| {
        let _actor = actor_id(&req)?;
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let slug = match &body.slug {
            Some(raw) => require_text(raw, "slug", MAX_NAME_LEN)?.to_lowercase(),
            None => slugify(&name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Slug must not be empty; provide a slug or a name with letters or digits",
            ));
        }
        if state.categories.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "A category with slug \"{slug}\" already exists"
            )));
        }
        if let Some(parent_id) = body.parent_id {
            if !state.categories.contains_key(&parent_id) {
                return Err(UnifiedError::not_found(format!(
                    "Parent category not found: {parent_id}"
                )));
            }
        }
        let now = Utc::now();
        let category = Category {
            id: Uuid::now_v7(),
            name,
            slug,
            description: optional_text(&body.description, "description", 1000)?,
            parent_id: body.parent_id,
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        Ok(category)
    })();
    match result {
        Ok(category) => created(category),
        Err(error) => err(error),
    }
}

async fn list_categories(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Category>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let parent_filter = params
            .filters
            .get("parent_id")
            .map(|raw| parse_uuid(raw, "parent category"))
            .transpose()?;
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Category> = state
            .categories
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|category| {
                if let Some(parent_id) = parent_filter {
                    if category.parent_id != Some(parent_id) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        category.name.to_lowercase(),
                        category.description.clone().unwrap_or_default().to_lowercase()
                    );
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "created_at", "updated_at"], "name");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a.created_at.cmp(&b.created_at),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.name.cmp(&b.name),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn get_category(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Category> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "category")?;
        state
            .categories
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {id}")))
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error),
    }
}

async fn update_category(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Category> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "category")?;
        let mut category = state
            .categories
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {id}")))?;
        if let Some(name) = &body.name {
            category.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = &body.slug {
            let slug = require_text(slug, "slug", MAX_NAME_LEN)?.to_lowercase();
            if state
                .categories
                .iter()
                .any(|entry| entry.value().slug == slug && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "A category with slug \"{slug}\" already exists"
                )));
            }
            category.slug = slug;
        }
        if let Some(description) = &body.description {
            category.description = match description {
                Some(text) => Some(require_text(text, "description", 1000)?),
                None => None,
            };
        }
        if let Some(parent_id) = body.parent_id {
            if let Some(parent_id) = parent_id {
                if parent_id == id {
                    return Err(UnifiedError::bad_request(
                        "A category cannot be its own parent",
                    ));
                }
                if !state.categories.contains_key(&parent_id) {
                    return Err(UnifiedError::not_found(format!(
                        "Parent category not found: {parent_id}"
                    )));
                }
            }
            category.parent_id = parent_id;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error),
    }
}

async fn delete_category(
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "category")?;
        if state
            .articles
            .iter()
            .any(|entry| entry.value().category_id == Some(id))
        {
            return Err(UnifiedError::conflict(format!(
                "Category {id} cannot be deleted because articles still use it"
            )));
        }
        if state.categories.remove(&id).is_none() {
            return Err(UnifiedError::not_found(format!("Category not found: {id}")));
        }
        Ok(json!({ "deleted": true, "category_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}
