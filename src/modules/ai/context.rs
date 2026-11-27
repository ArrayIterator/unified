//! ai module - Shared site context assembly and cost estimation.

use unified_core::hooks::{HookBus, HookContext};
use chrono::Utc;
use serde_json::{Value, json};

use super::constants::EVENT_KNOWLEDGEBASE_SEARCH;
use super::constants::EVENT_SITE_CONTEXT_REQUESTED;
use super::constants::GROUNDING_LIMIT;
use super::types::GroundingSource;
use super::types::SharedContext;
use super::types::SiteContextInput;

/// Parse `knowledgebase.search` hook results into grounding sources.
/// The knowledgebase handler contributes `{ "articles": [...] }`; any
/// other shape is ignored rather than trusted. (Same parsing contract
/// as the support assistant's `parse_sources`.)
pub fn parse_grounding_sources(ctx: &HookContext) -> Vec<GroundingSource> {
    let mut sources: Vec<GroundingSource> = Vec::new();
    for result in &ctx.results {
        let articles = match result.get("articles").and_then(|value| value.as_array()) {
            Some(articles) => articles.clone(),
            None => match result.as_array() {
                Some(articles) => articles.clone(),
                None => continue,
            },
        };
        for article in articles {
            let article_id = article
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let title = article
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if article_id.is_empty() || title.is_empty() {
                continue;
            }
            sources.push(GroundingSource {
                article_id,
                title,
                url: article
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                score: article
                    .get("score")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0),
                excerpt: article
                    .get("excerpt")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            });
        }
    }
    sources
}

/// Build the shared site context. `include_site_hooks` controls
/// whether `site.context_requested` is emitted to collect facts from
/// other modules: draft flows pass `true`; the
/// `site.context_requested` hook handler itself passes `false` so it
/// never re-emits the event it is answering.
pub fn assemble_shared_context(
    hooks: Option<&HookBus>,
    query: &str,
    site: &SiteContextInput,
    include_site_hooks: bool,
) -> SharedContext {
    let mut grounding_articles: Vec<GroundingSource> = Vec::new();
    let mut extra: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut site_name = site.site_name.clone();
    let mut site_url = site.site_url.clone();
    let mut locale = site.locale.clone();
    if let Some(bus) = hooks {
        if !query.trim().is_empty() {
            let ctx = bus.emit(
                EVENT_KNOWLEDGEBASE_SEARCH,
                json!({ "query": query, "limit": GROUNDING_LIMIT }),
            );
            grounding_articles = parse_grounding_sources(&ctx);
        }
        if include_site_hooks {
            let ctx = bus.emit(EVENT_SITE_CONTEXT_REQUESTED, json!({ "query": query }));
            for result in &ctx.results {
                if let Some(object) = result.as_object() {
                    for (key, value) in object {
                        match key.as_str() {
                            "site_name" if site_name.is_none() => {
                                site_name = value.as_str().map(str::to_string);
                            }
                            "site_url" if site_url.is_none() => {
                                site_url = value.as_str().map(str::to_string);
                            }
                            "locale" if locale.is_none() => {
                                locale = value.as_str().map(str::to_string);
                            }
                            "query" => {}
                            _ => {
                                extra.insert(key.clone(), value.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    SharedContext {
        site_name,
        site_url,
        locale,
        grounding_articles,
        extra,
        assembled_at: Utc::now(),
    }
}

/// Render the shared context as prompt text for a provider call.
pub(crate) fn shared_context_prompt(context: &SharedContext) -> String {
    let mut out = String::new();
    if let Some(name) = &context.site_name {
        out.push_str(&format!("Site name: {name}\n"));
    }
    if let Some(url) = &context.site_url {
        out.push_str(&format!("Site URL: {url}\n"));
    }
    if let Some(locale) = &context.locale {
        out.push_str(&format!("Locale: {locale}\n"));
    }
    if !context.grounding_articles.is_empty() {
        out.push_str("Knowledgebase grounding (published articles only):\n");
        for source in &context.grounding_articles {
            out.push_str(&format!("- {} ({})\n", source.title, source.url));
            if let Some(excerpt) = &source.excerpt {
                out.push_str(&format!("  {excerpt}\n"));
            }
        }
    }
    out
}

// Cost estimates.

/// Estimated USD cost from reported token counts, using list prices
/// per million tokens for known models. Returns `None` for unknown
/// models so no caller ever sees an invented price.
pub(crate) fn estimate_cost_usd(
    model: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> Option<f64> {
    // (prompt price, completion price) per million tokens.
    let (prompt_price, completion_price) = if model.starts_with("claude-sonnet-4") {
        (3.0, 15.0)
    } else if model.starts_with("claude-3-5-sonnet") {
        (3.0, 15.0)
    } else if model.starts_with("claude-3-haiku") {
        (0.25, 1.25)
    } else if model.starts_with("gpt-4o-mini") {
        (0.15, 0.60)
    } else if model.starts_with("gpt-4o") {
        (2.50, 10.0)
    } else if model.starts_with("gpt-3.5-turbo") {
        (0.50, 1.50)
    } else {
        return None;
    };
    let cost = (prompt_tokens as f64 * prompt_price + completion_tokens as f64 * completion_price)
        / 1_000_000.0;
    Some((cost * 1_000_000.0).round() / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::estimate_cost_usd;

    fn close(actual: Option<f64>, expected: f64) {
        let value = actual.expect("known model must have a price");
        assert!((value - expected).abs() < 1e-9, "{value} != {expected}");
    }

    #[test]
    fn cost_for_known_models() {
        close(estimate_cost_usd("gpt-4o-mini", 1_000_000, 0), 0.15);
        close(estimate_cost_usd("gpt-4o", 1_000_000, 1_000_000), 12.5);
        close(estimate_cost_usd("claude-3-haiku", 1_000_000, 0), 0.25);
        close(
            estimate_cost_usd("claude-sonnet-4-20250514", 2_000_000, 0),
            6.0,
        );
    }

    #[test]
    fn cost_unknown_model_is_none() {
        assert_eq!(estimate_cost_usd("mystery-model", 100, 100), None);
    }
}
