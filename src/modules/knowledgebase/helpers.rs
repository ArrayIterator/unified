//! Knowledgebase - Shared helpers.

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
use super::{constants::*, types::*, state::*, module::*, handlers_articles::*, handlers_tickets::*, handlers_categories::*};

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai and recruitment
/// modules. No route in this module trusts a bare `X-User-Id`
/// header on its own: every handler runs `require_permission`
/// against the caller's role before touching any state.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// Tenant edge guard (C3 pilot 3).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in, as a `Uuid`: the tenant
/// context's company first, the verified caller's server-resolved
/// company otherwise. Fails closed (403) when both are present and
/// disagree - a request must not carry one company in its tenant
/// context and another in its verified identity. The
/// `X-Company-Id` header is NEVER trusted here: only the verified
/// caller (company resolved from the database at authentication
/// time) and the middleware-resolved tenant context are sources
/// (header trust removal is handled elsewhere). A company
/// value that does not parse as a UUID names no tenant here and is
/// ignored, matching the previous `caller_company_id` behaviour.
///
/// This replaces the old `caller_company_id` helper (removed in
/// pilot 3): that helper also fell back to the
/// `user.company_requested` hook, which the guard path no longer
/// uses - a caller whose company the middleware did not resolve is
/// unresolved and reads global records only.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<Uuid>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty())
        .and_then(|raw| Uuid::parse_str(&raw).ok());
    let tenant_company = tenant
        .company_id
        .as_deref()
        .and_then(|raw| Uuid::parse_str(raw).ok());
    match (tenant_company, verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company)),
        (None, verified) => Ok(verified),
    }
}

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<Uuid>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3, rule A).
pub(crate) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<Uuid>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none() || record_company_id.as_ref() == Some(company),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<Uuid>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<Uuid>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) if record == caller => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Load an article and fail closed (403) when it is outside the
/// caller scope. Versions and ticket links scope through their
/// parent article with this.
pub(crate) fn require_article_in_scope(
    state: &KnowledgebaseState,
    id: Uuid,
    scope: &Option<Uuid>,
) -> UnifiedResult<Article> {
    let article = state
        .articles
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Article not found: {id}")))?;
    require_record_in_company_scope(&article.company_id, scope)?;
    Ok(article)
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

pub(crate) fn clean_tags(tags: Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
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

pub(crate) fn slugify(name: &str) -> String {
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

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

/// Build a short excerpt around the first query token found in the body.
pub(crate) fn excerpt(body: &str, tokens: &[String]) -> String {
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
///
/// Matching is on whole-token boundaries only: the query and every
/// field are split with [`tokenize`] and a token scores only when it
/// equals a field token. Substring matching would score "rust" on
/// "trusty" or "crusty", so `str::contains` / `str::matches` are
/// never used for matching here.
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
        let title_tokens = tokenize(&article.title);
        let body_tokens = tokenize(&article.body);
        let tag_tokens: Vec<String> = article
            .tags
            .iter()
            .flat_map(|tag| tokenize(tag))
            .collect();
        let mut score = 0f64;
        for token in &tokens {
            if title_tokens.iter().any(|field| field == token) {
                score += 3.0;
            }
            if tag_tokens.iter().any(|field| field == token) {
                score += 2.0;
            }
            let occurrences = body_tokens
                .iter()
                .filter(|field| *field == token)
                .count() as f64;
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

/// Tenant-scoped search for the hook handlers: only articles owned
/// by `company_id` are searched, published-only when requested.
///
/// Fails closed: `None` (the payload carried no caller company id)
/// returns no hits at all rather than every tenant's articles.
/// Articles with no company (`company_id: None`, created before
/// tenant partitioning or by a company-less caller) are never
/// returned here either, because no tenant can be proven to own
/// them. HTTP reads use the unscoped [`search_articles`] instead;
/// they are gated by `knowledgebase:view`, not by tenant.
pub fn search_articles_for_company(
    articles: &DashMap<Uuid, Article>,
    query: &str,
    only_published: bool,
    limit: usize,
    company_id: Option<Uuid>,
) -> Vec<ArticleSearchHit> {
    let Some(company_id) = company_id else {
        return Vec::new();
    };
    let scoped: DashMap<Uuid, Article> = DashMap::new();
    for entry in articles.iter() {
        if entry.value().company_id == Some(company_id) {
            scoped.insert(*entry.key(), entry.value().clone());
        }
    }
    search_articles(&scoped, query, only_published, limit)
}

// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `knowledgebase.ai.answer_draft_requested` hook (published
// articles only), and run through the AI module's gated runner.
// Drafts are returned inline and are never published by this module.

/// AI capabilities this module exposes through the AI module's
/// contract. The capability is non-PII: the question is redacted
/// before it reaches the model, and grounding uses published
/// articles only.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "answer_draft",
        module_id: "knowledgebase",
        name: "Draft an answer",
        description: "Draft an answer to a question, grounded in published knowledgebase articles. The output is returned inline; it is never published by this module.",
        input_fields: &["question"],
        allows_pii: false,
        use_case: "kb_answer",
    },
];

/// Run one registered knowledgebase AI capability:
/// `POST /api/v1/knowledgebase/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use`; delegates to the AI
/// module's gated runner.
pub(crate) async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let capability = path.into_inner();
    // Tenant edge guard (C3 pilot 3): fail closed without a
    // resolved tenant. Grounding stays scoped through the AI
    // runner's company resolution (published articles only,
    // tenant-filtered by the hook search).
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error),
    };
    let caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error),
    };
    if let Err(error) = ai::require_permission(&ctx, &caller.role, ai::PERM_AI_USE) {
        return err(error);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &caller);
    match ai::run_ai_capability(
        &ctx,
        &caller,
        company_id,
        "knowledgebase",
        &capability,
        body.into_inner().input,
    )
    .await
    {
        Ok(data) => ok(data),
        Err(error) => err(error),
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
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
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

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  --Hi--  "), "hi");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("already-slug"), "already-slug");
    }

    #[test]
    fn clean_tags_cases() {
        assert_eq!(
            clean_tags(Some(vec![
                "Rust".to_string(),
                " rust ".to_string(),
                "RUST".to_string(),
                "Guides".to_string(),
            ]))
            .expect("deduped"),
            vec!["rust".to_string(), "guides".to_string()]
        );
        assert_eq!(clean_tags(None).expect("none"), Vec::<String>::new());
        assert!(clean_tags(Some(vec!["".to_string()])).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("tag{i}")).collect();
        assert!(clean_tags(Some(many)).is_err());
    }

    #[test]
    fn tokenize_cases() {
        assert_eq!(tokenize("Hello, World! 123"), vec!["hello", "world", "123"]);
        assert!(tokenize("").is_empty());
        assert!(tokenize("   !!!   ").is_empty());
        assert_eq!(tokenize("a-b_c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn excerpt_cases() {
        assert_eq!(
            excerpt("hello world", &["world".to_string()]),
            "hello world"
        );
        assert_eq!(excerpt("hello world", &["missing".to_string()]), "hello world");
        let body = format!("{} needle {}", "a".repeat(300), "b".repeat(300));
        let text = excerpt(&body, &["needle".to_string()]);
        assert!(text.starts_with("..."));
        assert!(text.ends_with("..."));
        assert!(text.contains("needle"));
        let long = "x".repeat(500);
        assert!(excerpt(&long, &[]).ends_with("..."));
        assert_eq!(excerpt(&long, &[]).len(), 243);
    }

    #[test]
    fn require_and_optional_text_cases() {
        assert_eq!(require_text("  hi ", "title", 10).expect("ok"), "hi");
        assert!(require_text("", "title", 10).is_err());
        assert!(require_text("x".repeat(11).as_str(), "title", 10).is_err());
        assert_eq!(optional_text(&None, "title", 10).expect("none"), None);
        assert_eq!(
            optional_text(&Some("  x ".to_string()), "title", 10).expect("some"),
            Some("x".to_string())
        );
    }

    fn article(
        id: uuid::Uuid,
        title: &str,
        body: &str,
        status: ArticleStatus,
    ) -> Article {
        article_for_company(id, title, body, status, None)
    }

    fn article_for_company(
        id: uuid::Uuid,
        title: &str,
        body: &str,
        status: ArticleStatus,
        company_id: Option<uuid::Uuid>,
    ) -> Article {
        Article {
            id,
            title: title.to_string(),
            body: body.to_string(),
            category_id: None,
            status,
            tags: Vec::new(),
            company_id,
            version: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn search_articles_ranks_title_above_body() {
        let articles = dashmap::DashMap::new();
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let c = uuid::Uuid::new_v4();
        articles.insert(a, article(a, "rust guide", "nothing here", ArticleStatus::Published));
        articles.insert(b, article(b, "cooking", "rust", ArticleStatus::Published));
        articles.insert(c, article(c, "rust draft", "rust", ArticleStatus::Draft));
        let hits = search_articles(&articles, "rust", true, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, a);
        assert_eq!(hits[0].score, 3.0);
        assert_eq!(hits[1].id, b);
        assert_eq!(hits[1].score, 1.0);
        // Drafts are included when only_published is false.
        assert_eq!(search_articles(&articles, "rust", false, 10).len(), 3);
        // The limit is honoured.
        assert_eq!(search_articles(&articles, "rust", true, 1).len(), 1);
        // Empty query and no match return nothing.
        assert!(search_articles(&articles, "", true, 10).is_empty());
        assert!(search_articles(&articles, "zzz", true, 10).is_empty());
    }

    #[test]
    fn search_articles_matches_token_boundaries_only() {
        let articles = dashmap::DashMap::new();
        let tricky = uuid::Uuid::new_v4();
        let exact = uuid::Uuid::new_v4();
        let tagged = uuid::Uuid::new_v4();
        // "trusty", "crusty" and "rustacean" all contain "rust" as a
        // substring, but none of them is the token "rust".
        articles.insert(
            tricky,
            article(
                tricky,
                "trusty crusty guide",
                "trusty crusty rustacean",
                ArticleStatus::Published,
            ),
        );
        articles.insert(
            exact,
            article(exact, "rust guide", "rust rust", ArticleStatus::Published),
        );
        let mut tagged_article = article(
            tagged,
            "cooking",
            "nothing relevant",
            ArticleStatus::Published,
        );
        tagged_article.tags = vec!["trusty".to_string()];
        articles.insert(tagged, tagged_article);
        let hits = search_articles(&articles, "rust", true, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, exact);
        // Title token (3) + two body tokens (2) = 5.
        assert_eq!(hits[0].score, 5.0);
        // A whole-token tag still scores: exact tag "rust" is 2 points.
        let mut tagged_exact = article(
            tagged,
            "cooking",
            "nothing relevant",
            ArticleStatus::Published,
        );
        tagged_exact.tags = vec!["rust".to_string()];
        articles.insert(tagged, tagged_exact);
        let hits = search_articles(&articles, "rust", true, 10);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|hit| hit.id == tagged && hit.score == 2.0));
    }

    #[test]
    fn search_articles_for_company_filters_by_tenant_and_fails_closed() {
        let company_a = uuid::Uuid::new_v4();
        let company_b = uuid::Uuid::new_v4();
        let articles = dashmap::DashMap::new();
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let legacy = uuid::Uuid::new_v4();
        articles.insert(
            a,
            article_for_company(a, "rust guide", "rust", ArticleStatus::Published, Some(company_a)),
        );
        articles.insert(
            b,
            article_for_company(b, "rust guide", "rust", ArticleStatus::Published, Some(company_b)),
        );
        articles.insert(
            legacy,
            article(legacy, "rust guide", "rust", ArticleStatus::Published),
        );
        // Each tenant sees only its own articles.
        let hits = search_articles_for_company(&articles, "rust", true, 10, Some(company_a));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a);
        let hits = search_articles_for_company(&articles, "rust", true, 10, Some(company_b));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, b);
        // No caller company id: fail closed, nothing is returned --
        // never the whole store. The same holds for an unknown tenant.
        assert!(search_articles_for_company(&articles, "rust", true, 10, None).is_empty());
        assert!(
            search_articles_for_company(&articles, "rust", true, 10, Some(uuid::Uuid::new_v4()))
                .is_empty()
        );
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = verified_request("user-1", "admin");
        let caller = caller(&req).expect("both headers present");
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "admin");
        // A bare X-User-Id header is not enough: the role is
        // required, otherwise no capability can be checked.
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    fn test_app_context(allowed: &[&str]) -> std::sync::Arc<unified_core::context::AppContext> {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        let hooks = std::sync::Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed.iter().map(|name| name.to_string()).collect();
        hooks.register(
            "role.check",
            "knowledgebase-test",
            std::sync::Arc::new(move |ctx: &mut unified_core::hooks::HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        std::sync::Arc::new(unified_core::context::AppContext::new(
            AppConfig::default(),
            hooks,
            std::sync::Arc::new(ModuleRegistry::new()),
        ))
    }

    #[test]
    fn check_capability_fails_closed_without_an_answer() {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        // No role.check handler at all: nothing answers, so nothing
        // is allowed -- the gate never defaults to open.
        let ctx = unified_core::context::AppContext::new(
            AppConfig::default(),
            std::sync::Arc::new(HookBus::new()),
            std::sync::Arc::new(ModuleRegistry::new()),
        );
        assert!(!check_capability(&ctx, "admin", PERM_KNOWLEDGEBASE_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_KNOWLEDGEBASE_MANAGE).is_err());
        // A handler that allows only view: manage stays denied.
        let ctx = test_app_context(&[PERM_KNOWLEDGEBASE_VIEW]);
        assert!(check_capability(&ctx, "editor", PERM_KNOWLEDGEBASE_VIEW));
        assert!(!check_capability(&ctx, "editor", PERM_KNOWLEDGEBASE_MANAGE));
        assert!(require_permission(&ctx, "editor", PERM_KNOWLEDGEBASE_VIEW).is_ok());
        assert!(require_permission(&ctx, "editor", PERM_KNOWLEDGEBASE_MANAGE).is_err());
        let ctx = test_app_context(&[PERM_KNOWLEDGEBASE_VIEW, PERM_KNOWLEDGEBASE_MANAGE]);
        assert!(require_permission(&ctx, "admin", PERM_KNOWLEDGEBASE_MANAGE).is_ok());
    }

    #[test]
    fn caller_company_prefers_tenant_then_verified_caller_and_ignores_headers() {
        // Pilot 3 replaced `caller_company_id` (verified caller,
        // then the `user.company_requested` hook) with
        // `caller_company` (tenant context, then the verified
        // caller). The old test asserted that an `X-Company-Id`
        // header stamps the company; the header was never trusted
        // in code, so that assertion could not pass. The header
        // is ignored here too.
        use actix_web::test::TestRequest;
        let company = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let tenant_request = |tenant_company: &str, caller_company: Option<&str>| {
            let req = TestRequest::default()
                .insert_header(("x-company-id", other.to_string()))
                .to_http_request();
            {
                use actix_web::HttpMessage;
                req.extensions_mut()
                    .insert(unified_core::middleware::TenantContext {
                        tenant_id: uuid::Uuid::now_v7().to_string(),
                        company_id: tenant_company.to_string(),
                        site_id: String::new(),
                        kind: unified_core::middleware::TenantKind::Subdomain,
                        host: "tenant.example.test".to_string(),
                        scope: None,
                        product_id: None,
                        package_id: None,
                        product_subdomain_id: None,
                    });
                let mut caller = unified_core::auth::Caller::verified(
                    test_user_uuid("user-1"),
                    Vec::new(),
                )
                .with_roles(vec!["admin".to_string()]);
                if let Some(company) = caller_company {
                    caller = caller.with_company(company.to_string());
                }
                req.extensions_mut().insert(caller);
            }
            req
        };
        // Tenant company wins; the forged header is ignored.
        let req = tenant_request(&company.to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
        // Tenant and verified caller disagree: fail closed.
        let req = tenant_request(&company.to_string(), Some(&other.to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
        // No tenant company: the verified caller's company wins.
        let req = tenant_request("", Some(&company.to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
        // Neither: no scope, even with the header present.
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
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

    fn test_article(company_id: Option<Uuid>) -> Article {
        let now = Utc::now();
        Article {
            id: Uuid::now_v7(),
            title: "Guide".to_string(),
            body: "body".to_string(),
            category_id: None,
            status: ArticleStatus::Published,
            tags: Vec::new(),
            company_id,
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let company = Uuid::new_v4();
        let req = tenant_request(&company.to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let company = Uuid::new_v4();
        let req = tenant_request("", Some(&company.to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &Uuid::new_v4().to_string(),
            Some(&Uuid::new_v4().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_shows_global_records_to_scoped_callers() {
        let company = Uuid::new_v4();
        let other = Uuid::new_v4();
        let scope = Some(company);
        assert!(in_company_scope(&Some(company), &scope));
        assert!(!in_company_scope(&Some(other), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(other), &None));
    }

    #[test]
    fn require_article_in_scope_hides_other_companies_articles() {
        let state = KnowledgebaseState::default();
        let company = Uuid::new_v4();
        let other = Uuid::new_v4();
        let own = test_article(Some(company));
        let foreign = test_article(Some(other));
        let global = test_article(None);
        state.articles.insert(own.id, own.clone());
        state.articles.insert(foreign.id, foreign.clone());
        state.articles.insert(global.id, global.clone());
        let scope = Some(company);
        assert!(require_article_in_scope(&state, own.id, &scope).is_ok());
        assert!(require_article_in_scope(&state, foreign.id, &scope).is_err());
        assert!(require_article_in_scope(&state, global.id, &scope).is_err());
        // An unresolved caller reads global articles only.
        assert!(require_article_in_scope(&state, global.id, &None).is_ok());
        assert!(require_article_in_scope(&state, own.id, &None).is_err());
        // A missing article is a 404 either way.
        assert!(require_article_in_scope(&state, Uuid::new_v4(), &scope).is_err());
    }
}
