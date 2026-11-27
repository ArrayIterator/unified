//! Knowledgebase - Module definition: routes, hooks, and registration.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
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
use super::{constants::*, types::*, state::*, helpers::*, handlers_articles::*, handlers_tickets::*, handlers_categories::*};

// Module

pub struct KnowledgebaseModule {
    pub(crate) state: KnowledgebaseState,
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

/// The caller company id carried by a hook payload, if any.
///
/// Two shapes are accepted, because the emitters disagree: a
/// top-level `"company_id"` string, and the nested
/// `"caller": { "company_id": ... }` shape the AI runner emits for
/// grounding events (see `run_ai_capability`, step (d)). A missing,
/// null, or unparsable value yields `None` -- the caller is
/// company-less, and the hook handlers fail closed on `None`.
/// No new payload field is invented here: only fields an emitter
/// actually sends are read.
pub(crate) fn hook_company_id(ctx: &HookContext) -> Option<Uuid> {
    let raw = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            ctx.get("caller")
                .and_then(|caller| caller.get("company_id"))
                .and_then(|value| value.as_str())
        })?;
    Uuid::parse_str(raw).ok()
}

/// Build the `{"articles": [...]}` result for a hook search,
/// tenant-scoped and published-only. Returns an empty list when no
/// caller company id is present (fail closed), so a hook caller can
/// never read another tenant's articles by omitting the tenant.
fn hook_search_articles(
    state: &KnowledgebaseState,
    query: &str,
    limit: usize,
    company_id: Option<Uuid>,
) -> Value {
    let hits = search_articles_for_company(&state.articles, query, true, limit, company_id);
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
    json!({ "articles": articles })
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
        ai::register_ai_capabilities("knowledgebase", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Answer `knowledgebase.search` for AI Support (and any other
        // module): grounded answers must come from real articles.
        //
        // Tenant scoping: the handler returns only the calling
        // tenant's published articles, filtered by the caller
        // company id carried in the payload (see
        // `hook_company_id`). The payloads the current emitters
        // send carry no company id at all -- tickets sends
        // `{ "query", "ticket_id" }`, AI Support sends
        // `{ "query", "limit" }`, and the MCP tool sends
        // `{ "query", "limit" }` -- so this handler fails closed
        // for them today and returns no articles rather than every
        // tenant's articles. Once those emitters include a
        // `company_id` (top level) or `caller.company_id` in the
        // payload, this handler filters by it with no code change
        // here; the `Article.company_id` column that makes the
        // filter possible is already recorded at creation time.
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
                ctx.add_result(hook_search_articles(
                    &state,
                    &query,
                    limit,
                    hook_company_id(ctx),
                ));
            }),
        );
        // Ground the `answer_draft` AI capability with the same
        // published-only search the `knowledgebase.search` hook
        // uses, so answers are grounded in real articles. The AI
        // runner emits this event with the caller company id nested
        // as `caller.company_id` (null for control-plane calls), so
        // unlike `knowledgebase.search` this path is tenant-scoped
        // today; a call with no company id fails closed and returns
        // no articles.
        let state = self.state.clone();
        bus.register(
            "knowledgebase.ai.answer_draft_requested",
            "knowledgebase",
            Arc::new(move |ctx| {
                let question = ctx
                    .get("input")
                    .and_then(|input| input.get("question"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if question.trim().is_empty() {
                    ctx.add_result(json!({ "articles": [] }));
                    return;
                }
                ctx.add_result(hook_search_articles(
                    &state,
                    &question,
                    5,
                    hook_company_id(ctx),
                ));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        // No route is public: every route requires a capability
        // checked through the `role.check` hook (reads need
        // `knowledgebase:view`, writes need `knowledgebase:manage`),
        // except the AI route, which requires `ai:use` through the
        // AI module's gated runner. There is no published-only
        // public read surface: even the search endpoint can return
        // drafts (`status=all`), so it stays behind `view`.
        vec![
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles", owner: "knowledgebase", description: "List articles (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/knowledgebase/articles", owner: "knowledgebase", description: "Create article (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}", owner: "knowledgebase", description: "Get article (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/knowledgebase/articles/{id}", owner: "knowledgebase", description: "Update article (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/knowledgebase/articles/{id}", owner: "knowledgebase", description: "Delete article (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}/versions", owner: "knowledgebase", description: "List versions (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}/versions/{version}", owner: "knowledgebase", description: "Get version (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/knowledgebase/articles/{id}/link-ticket", owner: "knowledgebase", description: "Link ticket (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/articles/{id}/tickets", owner: "knowledgebase", description: "List article tickets (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/knowledgebase/articles/{id}/tickets/{ticket_id}", owner: "knowledgebase", description: "Unlink ticket (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/categories", owner: "knowledgebase", description: "List categories (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/knowledgebase/categories", owner: "knowledgebase", description: "Create category (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/categories/{id}", owner: "knowledgebase", description: "Get category (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/knowledgebase/categories/{id}", owner: "knowledgebase", description: "Update category (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/knowledgebase/categories/{id}", owner: "knowledgebase", description: "Delete category (requires knowledgebase:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/knowledgebase/search", owner: "knowledgebase", description: "Search endpoint (requires knowledgebase:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/knowledgebase/ai/{capability}", owner: "knowledgebase", description: "Run one registered knowledgebase AI capability (answer_draft) (requires ai:use)", auth: unified_core::auth::RouteAuth::Authenticated},
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
                .route("/search", web::get().to(search_endpoint))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook_context(payload: Value) -> HookContext {
        HookContext::new("knowledgebase.search", payload)
    }

    #[test]
    fn hook_company_id_reads_both_payload_shapes() {
        let company = Uuid::new_v4();
        // Top-level company_id (the shape a future emitter sends).
        let ctx = hook_context(json!({ "query": "rust", "company_id": company.to_string() }));
        assert_eq!(hook_company_id(&ctx), Some(company));
        // Nested caller.company_id (the shape the AI runner sends
        // for knowledgebase.ai.answer_draft_requested).
        let ctx = hook_context(json!({
            "input": { "question": "how?" },
            "caller": { "user_id": "user-1", "company_id": company.to_string() },
        }));
        assert_eq!(hook_company_id(&ctx), Some(company));
        // Top level wins when both are present.
        let other = Uuid::new_v4();
        let ctx = hook_context(json!({
            "company_id": company.to_string(),
            "caller": { "company_id": other.to_string() },
        }));
        assert_eq!(hook_company_id(&ctx), Some(company));
    }

    #[test]
    fn hook_company_id_absent_or_invalid_is_none() {
        // The payloads the current knowledgebase.search emitters
        // send: tickets ({query, ticket_id}), AI Support and MCP
        // ({query, limit}) -- none of them carries a company id,
        // so the hook handlers fail closed on these.
        let ctx = hook_context(json!({ "query": "rust", "ticket_id": "T-1" }));
        assert_eq!(hook_company_id(&ctx), None);
        let ctx = hook_context(json!({ "query": "rust", "limit": 5 }));
        assert_eq!(hook_company_id(&ctx), None);
        // Null and unparsable values are not trusted as an id.
        let ctx = hook_context(json!({ "caller": { "company_id": null } }));
        assert_eq!(hook_company_id(&ctx), None);
        let ctx = hook_context(json!({ "company_id": "not-a-uuid" }));
        assert_eq!(hook_company_id(&ctx), None);
        let ctx = hook_context(json!({}));
        assert_eq!(hook_company_id(&ctx), None);
    }

    #[test]
    fn hook_search_articles_fails_closed_without_company() {
        let company = Uuid::new_v4();
        let state = KnowledgebaseState::default();
        let id = Uuid::now_v7();
        state.articles.insert(
            id,
            Article {
                id,
                title: "rust guide".to_string(),
                body: "rust".to_string(),
                category_id: None,
                status: ArticleStatus::Published,
                tags: Vec::new(),
                company_id: Some(company),
                version: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        // No company id: nothing is returned, even though a
        // matching published article exists.
        let result = hook_search_articles(&state, "rust", 5, None);
        assert_eq!(result["articles"].as_array().unwrap().len(), 0);
        // The owning tenant gets the hit.
        let result = hook_search_articles(&state, "rust", 5, Some(company));
        let articles = result["articles"].as_array().unwrap();
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0]["id"].as_str(), Some(id.to_string()).as_deref());
    }
}
