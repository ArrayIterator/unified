//! Knowledgebase - HTTP handlers for articles and search.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_tickets::*, handlers_categories::*};

// Article handlers

pub(crate) async fn create_article(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    body: web::Json<CreateArticleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Article> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_MANAGE)?;
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
            company_id: _scope,
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

pub(crate) async fn list_articles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Article>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
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
                if !in_company_scope(&article.company_id, &_scope) {
                    return false;
                }
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
                    // Whole-token matching only: a substring
                    // hit ("rust" inside "trusty") is not a match.
                    let haystack_tokens = tokenize(&format!(
                        "{} {} {}",
                        article.title, article.body, article.tags.join(" ")
                    ));
                    if !tokens.iter().any(|token| {
                        haystack_tokens.iter().any(|field| field == token)
                    }) {
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

pub(crate) async fn get_article(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Article> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        let article = state
            .articles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Article not found: {id}")))?;
        require_record_in_company_scope(&article.company_id, &_scope)?;
        Ok(article)
    })();
    match result {
        Ok(article) => ok(article),
        Err(error) => err(error),
    }
}

pub(crate) async fn update_article(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateArticleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Article> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        let mut article = state
            .articles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Article not found: {id}")))?;
        require_record_writable_in_company_scope(&article.company_id, &_scope)?;
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

pub(crate) async fn delete_article(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        require_article_in_scope(&state, id, &_scope)?;
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

pub(crate) async fn list_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ArticleVersion>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        require_article_in_scope(&state, id, &_scope)?;
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

pub(crate) async fn get_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<ArticleVersion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
        let (raw_id, raw_version) = path.into_inner();
        let id = parse_uuid(&raw_id, "article")?;
        let version: u32 = raw_version
            .parse()
            .map_err(|_| UnifiedError::bad_request(format!("Invalid version: {raw_version}")))?;
        require_article_in_scope(&state, id, &_scope)?;
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

// Search handler

pub(crate) async fn search_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    query: web::Query<SearchQuery>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ArticleSearchHit>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
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
        // Scope the hits: company callers see their own and
        // global articles only; unresolved callers see global
        // articles only.
        hits.retain(|hit| {
            state
                .articles
                .get(&hit.id)
                .is_some_and(|entry| in_company_scope(&entry.value().company_id, &_scope))
        });
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

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead. The request also carries the
    /// middleware-resolved tenant context (no company), so the
    /// tenant gate passes and the caller scope is unresolved
    /// (global records only).
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req.extensions_mut().insert(tenant_context());
        req
    }

    /// The middleware-resolved tenant context the tests attach:
    /// a tenant with no company, so the caller scope comes from
    /// the verified caller alone.
    fn tenant_context() -> unified_core::middleware::TenantContext {
        unified_core::middleware::TenantContext {
            tenant_id: uuid::Uuid::now_v7().to_string(),
            company_id: String::new(),
            site_id: String::new(),
            kind: unified_core::middleware::TenantKind::Subdomain,
            host: "tenant.example.test".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        }
    }

    /// A verified caller whose server-resolved company is
    /// `company`, with the tenant context attached.
    fn verified_request_with_company(
        user_id: &str,
        role: &str,
        company: uuid::Uuid,
    ) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()])
                .with_company(company.to_string()),
        );
        req.extensions_mut().insert(tenant_context());
        req
    }

    /// A verified caller with no tenant context attached: the
    /// tenant gate must reject the request before any permission
    /// check runs.
    fn tenantless_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
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
    use actix_web::test::TestRequest;

    /// AppContext whose `role.check` hook allows exactly the given
    /// permissions, so handler tests exercise the gate, not the
    /// roles module (same shape as the properties module's tests).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed.iter().map(|name| name.to_string()).collect();
        hooks.register(
            "role.check",
            "knowledgebase-test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn test_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn create_body() -> web::Json<CreateArticleRequest> {
        web::Json(CreateArticleRequest {
            title: "Rust guide".to_string(),
            body: "rust".to_string(),
            category_id: None,
            status: None,
            tags: None,
        })
    }

    #[tokio::test]
    async fn create_article_requires_manage_permission() {
        let state = web::Data::new(KnowledgebaseState::default());
        // A caller holding only view cannot create: 403, and
        // nothing is stored.
        let resp = create_article(
            test_ctx(&[PERM_KNOWLEDGEBASE_VIEW]),
            state.clone(),
            test_request(),
            create_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(state.articles.len(), 0);
        // A caller holding manage can create: 201, and the article
        // is stored under the caller's verified company.
        let company = Uuid::new_v4();
        let resp = create_article(
            test_ctx(&[PERM_KNOWLEDGEBASE_MANAGE]),
            state.clone(),
            verified_request_with_company("user-1", "admin", company),
            create_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(state.articles.len(), 1);
        let stored = state.articles.iter().next().unwrap().value().clone();
        assert_eq!(stored.company_id, Some(company));
        // A caller holding manage but with no company creates a
        // global article (company_id None), never another
        // company's.
        let resp = create_article(
            test_ctx(&[PERM_KNOWLEDGEBASE_MANAGE]),
            state.clone(),
            test_request(),
            create_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(state.articles.len(), 2);
        assert_eq!(
            state
                .articles
                .iter()
                .filter(|entry| entry.value().company_id.is_none())
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn reads_require_tenant_view_and_a_verified_caller() {
        let state = web::Data::new(KnowledgebaseState::default());
        // No tenant context at all: 401, before any permission
        // check, even for a verified caller holding view.
        let resp = list_articles(
            test_ctx(&[PERM_KNOWLEDGEBASE_VIEW]),
            state.clone(),
            tenantless_request("user-1", "admin"),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // Tenant context but no verified caller: 401 as well.
        let req = actix_web::test::TestRequest::default().to_http_request();
        {
            use actix_web::HttpMessage;
            req.extensions_mut().insert(tenant_context());
        }
        let resp = list_articles(
            test_ctx(&[PERM_KNOWLEDGEBASE_VIEW]),
            state.clone(),
            req,
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // A role holding neither capability cannot read: 403.
        let resp = list_articles(
            test_ctx(&[]),
            state.clone(),
            test_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        // A role holding view can read: 200.
        let resp = list_articles(
            test_ctx(&[PERM_KNOWLEDGEBASE_VIEW]),
            state.clone(),
            test_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_articles_hides_other_companies_articles() {
        let state = web::Data::new(KnowledgebaseState::default());
        let ctx = test_ctx(&[PERM_KNOWLEDGEBASE_MANAGE, PERM_KNOWLEDGEBASE_VIEW]);
        let company_a = Uuid::new_v4();
        let company_b = Uuid::new_v4();
        // One article per company, plus one global article.
        let resp = create_article(
            ctx.clone(),
            state.clone(),
            verified_request_with_company("user-a", "admin", company_a),
            create_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp = create_article(
            ctx.clone(),
            state.clone(),
            verified_request_with_company("user-b", "admin", company_b),
            create_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp = create_article(ctx.clone(), state.clone(), test_request(), create_body()).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(state.articles.len(), 3);
        // A company-A caller lists its own article plus the
        // global one - never company B's.
        let resp = list_articles(
            ctx.clone(),
            state.clone(),
            verified_request_with_company("user-a", "admin", company_a),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = actix_web::body::to_bytes(resp.into_body())
            .await
            .expect("list body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(&text).expect("list json");
        let items = value
            .get("data")
            .and_then(|data| data.get("items"))
            .and_then(|items| items.as_array())
            .expect("list items");
        assert_eq!(items.len(), 2);
        let companies: Vec<Option<String>> = items
            .iter()
            .map(|item| {
                item.get("company_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert!(companies.contains(&Some(company_a.to_string())));
        assert!(companies.contains(&None));
        assert!(!companies.contains(&Some(company_b.to_string())));
        // Company B's article is a 403 for a company-A caller.
        let foreign_id = state
            .articles
            .iter()
            .find(|entry| entry.value().company_id == Some(company_b))
            .expect("company B article")
            .key()
            .to_string();
        let resp = get_article(
            ctx.clone(),
            state.clone(),
            verified_request_with_company("user-a", "admin", company_a),
            web::Path::from(foreign_id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_articles_search_matches_whole_tokens_only() {
        let state = web::Data::new(KnowledgebaseState::default());
        let ctx = test_ctx(&[PERM_KNOWLEDGEBASE_MANAGE, PERM_KNOWLEDGEBASE_VIEW]);
        // Seed one article whose title/body only contain "rust" as
        // a substring ("trusty"), and one with the real token.
        let tricky = Uuid::now_v7();
        state.articles.insert(
            tricky,
            Article {
                id: tricky,
                title: "trusty guide".to_string(),
                body: "trusty crusty".to_string(),
                category_id: None,
                status: ArticleStatus::Published,
                tags: Vec::new(),
                company_id: None,
                version: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        let resp = create_article(
            ctx.clone(),
            state.clone(),
            test_request(),
            create_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let mut params = PaginationParams::default();
        params.search = Some("rust".to_string());
        let resp = list_articles(ctx, state.clone(), test_request(), web::Query(params)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        // Only the article with the whole token "rust" survives
        // the filter; the "trusty" article does not.
        assert_eq!(state.articles.len(), 2);
        let body = actix_web::body::to_bytes(resp.into_body())
            .await
            .expect("list body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(text.contains("Rust guide"));
        assert!(!text.contains("trusty guide"));
    }
}
