// Knowledgebase module.
//
// Articles with full version history, categories, token-based full-text
// search, and ticket links. The search index is also exposed to other
// modules through the `knowledgebase.search` hook event so the AI Support
// assistant can ground its answers in real articles instead of inventing
// them.
//
// Auth: no route is public. Every handler requires `X-User-Id` and
// `X-Role` headers and a capability checked through the `role.check`
// hook (fail closed): reads need `knowledgebase:view`, writes need
// `knowledgebase:manage`; the AI route needs `ai:use` through the AI
// module's gated runner. Articles record the owning tenant
// (`Article.company_id`, resolved at creation from the verified
// caller and the tenant context — never from the `X-Company-Id`
// header); the hook searches below return only that tenant's
// published articles.
//
// Routes live under `/api/v1/knowledgebase`:
//   - `GET/POST  /articles` -- list (paginated, filterable) / create.
//   - `GET/PATCH/DELETE /articles/{id}` -- read / update / delete.
//   - `GET /articles/{id}/versions` and `GET /articles/{id}/versions/{v}`.
//   - `GET/POST  /categories`, `GET/PATCH/DELETE /categories/{id}`.
//   - `GET /search?q=...` -- ranked full-text search.
//   - `POST /articles/{id}/link-ticket`, `GET/DELETE /articles/{id}/tickets`.
//
// Hook contract for `knowledgebase.search` (emitted by AI Support):
//   - Input payload: `{ "query": "<text>", "limit": <number> }`.
//   - This handler appends one result: `{ "articles": [...] }` where each
//     article is `{ "id", "title", "excerpt", "score", "url" }` and only
//     published articles are returned.
//   - Tenant scoping: the handler returns only the calling tenant's
//     articles, filtered by the caller company id in the payload
//     (`company_id` top level, or `caller.company_id`). The payloads
//     the current emitters send carry no company id, so the handler
//     fails closed and returns no articles until those emitters
//     include one; it never returns every tenant's articles.
//
// Hook contract for `knowledgebase.ai.answer_draft_requested`
// (emitted by the AI runner for the `answer_draft` capability):
//   - Input payload: `{ "input": { "question": "<text>" },
//     "caller": { "user_id": ..., "company_id": "<uuid>" | null } }`.
//   - Same published-only, tenant-scoped result shape as above; a
//     call with no company id returns no articles.

mod constants;
pub mod types;
pub mod state;
pub mod helpers;
pub mod module;
mod handlers_articles;
mod handlers_tickets;
mod handlers_categories;

pub use self::types::{ArticleStatus, Article, ArticleVersion, Category, TicketLink, ArticleSearchHit, CreateArticleRequest, UpdateArticleRequest, CreateCategoryRequest, UpdateCategoryRequest, LinkTicketRequest, SearchQuery};
pub use self::state::{KnowledgebaseState};
pub use self::helpers::{search_articles, AI_CAPABILITIES};
pub use self::module::{KnowledgebaseModule, register};
