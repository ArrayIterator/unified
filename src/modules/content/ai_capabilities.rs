//! Content Management module. - AI capabilities (AI Capabilities Contract, see `crate::modules::ai`). Declared here, registered once in `init`, grounded through the `content.ai.<capability>_requested` hooks, and run through the AI module's gated runner. Drafts are returned inline and are never published by this module.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.

use super::categories::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::json as api_json;
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

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
pub(super) fn related_content(state: &ContentState, query: &str) -> Vec<Value> {
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
pub(super) async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
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
