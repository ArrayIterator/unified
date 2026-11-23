// Site Builder — Server-side rendering of blocks and pages.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::handlers::*;
use super::module::*;

/// Render one block to its default HTML fragment. The Theme module may
/// replace the fragment through the `theme.render_block` event (see
/// `render_block`).
pub(crate) fn render_block_default(block: &Block) -> String {
    let content = &block.content;
    match block.block_type.as_str() {
        "hero" => {
            let heading = content_str(content, "heading").unwrap_or_default();
            let subheading = content_str(content, "subheading").unwrap_or_default();
            let image_url = content_str(content, "image_url").unwrap_or_default();
            let cta_text = content_str(content, "cta_text").unwrap_or_default();
            let cta_url = content_str(content, "cta_url").unwrap_or_default();
            let mut html = String::from("<section class=\"sb-hero\">");
            if is_safe_url(&image_url) {
                html.push_str(&format!(
                    "<img class=\"sb-hero-image\" src=\"{}\" alt=\"{}\">",
                    escape_html(&image_url),
                    escape_html(&heading)
                ));
            }
            if !heading.is_empty() {
                html.push_str(&format!("<h1>{}</h1>", escape_html(&heading)));
            }
            if !subheading.is_empty() {
                html.push_str(&format!("<p class=\"sb-hero-sub\">{}</p>", escape_html(&subheading)));
            }
            if !cta_text.is_empty() {
                // An unsafe CTA target degrades to "#" instead of
                // reaching the page (see `is_safe_url`).
                let url = safe_url(&cta_url, "#");
                html.push_str(&format!(
                    "<a class=\"sb-hero-cta\" href=\"{}\">{}</a>",
                    escape_html(&url),
                    escape_html(&cta_text)
                ));
            }
            html.push_str("</section>");
            html
        }
        "text" => {
            // `html` is admin-authored template content and is passed
            // through; `text` is always escaped.
            if let Some(html) = content_str(content, "html") {
                format!("<div class=\"sb-text\">{html}</div>")
            } else {
                let text = content_str(content, "text").unwrap_or_default();
                format!("<div class=\"sb-text\"><p>{}</p></div>", escape_html(&text))
            }
        }
        "image" => {
            let url = content_str(content, "url").unwrap_or_default();
            let alt = content_str(content, "alt").unwrap_or_default();
            let caption = content_str(content, "caption").unwrap_or_default();
            // An unsafe image URL renders no <img> at all rather than
            // a dangerous `src`.
            let mut html = String::from("<figure class=\"sb-image\">");
            if is_safe_url(&url) {
                html.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\">",
                    escape_html(&url),
                    escape_html(&alt)
                ));
            }
            if !caption.is_empty() {
                html.push_str(&format!("<figcaption>{}</figcaption>", escape_html(&caption)));
            }
            html.push_str("</figure>");
            html
        }
        "gallery" => {
            let mut html = String::from("<div class=\"sb-gallery\">");
            if let Some(images) = content.get("images").and_then(|v| v.as_array()) {
                for image in images {
                    let url = content_str(image, "url").unwrap_or_default();
                    let alt = content_str(image, "alt").unwrap_or_default();
                    if is_safe_url(&url) {
                        html.push_str(&format!(
                            "<img src=\"{}\" alt=\"{}\">",
                            escape_html(&url),
                            escape_html(&alt)
                        ));
                    }
                }
            }
            html.push_str("</div>");
            html
        }
        "form" => {
            // An unsafe form target degrades to "#" instead of
            // reaching the page (see `is_safe_url`).
            let action = content_str(content, "action")
                .map(|action| safe_url(&action, "#"))
                .unwrap_or_else(|| "#".to_string());
            let submit_label =
                content_str(content, "submit_label").unwrap_or_else(|| "Submit".to_string());
            let mut html = format!(
                "<form class=\"sb-form\" action=\"{}\" method=\"post\">",
                escape_html(&action)
            );
            if let Some(fields) = content.get("fields").and_then(|v| v.as_array()) {
                for field in fields {
                    let name = content_str(field, "name").unwrap_or_default();
                    let label = content_str(field, "label").unwrap_or_else(|| name.clone());
                    let field_type = content_str(field, "type").unwrap_or_else(|| "text".to_string());
                    let placeholder = content_str(field, "placeholder").unwrap_or_default();
                    let required = content_bool(field, "required");
                    let required_attr = if required { " required" } else { "" };
                    html.push_str(&format!(
                        "<div class=\"sb-field\"><label for=\"{}\">{}</label>",
                        escape_html(&name),
                        escape_html(&label)
                    ));
                    match field_type.as_str() {
                        "textarea" => html.push_str(&format!(
                            "<textarea id=\"{}\" name=\"{}\" placeholder=\"{}\"{}></textarea>",
                            escape_html(&name),
                            escape_html(&name),
                            escape_html(&placeholder),
                            required_attr
                        )),
                        "select" => {
                            html.push_str(&format!(
                                "<select id=\"{}\" name=\"{}\"{}>",
                                escape_html(&name),
                                escape_html(&name),
                                required_attr
                            ));
                            if let Some(options) = field.get("options").and_then(|v| v.as_array()) {
                                for option in options {
                                    let value = match option {
                                        Value::String(text) => text.clone(),
                                        other => other.to_string(),
                                    };
                                    html.push_str(&format!(
                                        "<option value=\"{}\">{}</option>",
                                        escape_html(&value),
                                        escape_html(&value)
                                    ));
                                }
                            }
                            html.push_str("</select>");
                        }
                        "checkbox" => html.push_str(&format!(
                            "<input type=\"checkbox\" id=\"{}\" name=\"{}\"{}>",
                            escape_html(&name),
                            escape_html(&name),
                            required_attr
                        )),
                        other => html.push_str(&format!(
                            "<input type=\"{}\" id=\"{}\" name=\"{}\" placeholder=\"{}\"{}>",
                            escape_html(other),
                            escape_html(&name),
                            escape_html(&name),
                            escape_html(&placeholder),
                            required_attr
                        )),
                    }
                    html.push_str("</div>");
                }
            }
            html.push_str(&format!(
                "<button type=\"submit\">{}</button></form>",
                escape_html(&submit_label)
            ));
            html
        }
        "custom" => content_str(content, "html").unwrap_or_default(),
        "pricing_table" => {
            // Placeholder only: the page fetches the structured data
            // from the pricing tables render endpoint. The id is
            // escaped; an empty id still renders an empty placeholder
            // rather than leaking anything.
            let table_id = content_str(content, "pricing_table_id")
                .or_else(|| content_str(content, "table_id"))
                .unwrap_or_default();
            format!(
                "<section class=\"sb-pricing-table\" data-pricing-table-id=\"{}\" data-render-url=\"/api/v1/pricing-tables/tables/{}/render\"></section>",
                escape_html(&table_id),
                escape_html(&table_id)
            )
        }
        "marketplace_storefront" => {
            // Placeholder only: the page fetches the storefront data
            // from the marketplace storefront endpoint.
            let vendor_slug = content_str(content, "vendor_slug")
                .or_else(|| content_str(content, "vendor_id"))
                .unwrap_or_default();
            format!(
                "<section class=\"sb-marketplace-storefront\" data-vendor-slug=\"{}\" data-storefront-url=\"/api/v1/pricing-tables/marketplace/storefronts/{}\"></section>",
                escape_html(&vendor_slug),
                escape_html(&vendor_slug)
            )
        }
        other => format!("<!-- unknown block type: {} -->", escape_html(other)),
    }
}

/// Render one block, letting the Theme module replace the fragment through
/// the `theme.render_block` event.
pub(crate) fn render_block(state: &SiteBuilderState, page: &Page, block: &Block) -> String {
    let default_html = render_block_default(block);
    if let Some(hooks) = state.hooks() {
        let answer = hooks.emit(
            EVENT_THEME_RENDER_BLOCK,
            json!({
                "page_id": page.id,
                "block_id": block.id,
                "block_type": block.block_type,
                "content": block.content,
                "html": default_html,
            }),
        );
        if let Some(html) = answer.data.get("html").and_then(|v| v.as_str()) {
            return html.to_string();
        }
    }
    default_html
}

/// Answer the `site_builder.render_page` hook: render a page by slug
/// (or id) to HTML through the block renderer. Only published pages
/// render — a draft answers `found: false`, so it can never leak to
/// the public front page. The tagline and description come from the
/// page's first hero block, so the front page's words are builder
/// content, not hard-coded strings.
pub(crate) fn on_render_page_request(state: &SiteBuilderState, ctx: &mut HookContext) {
    let slug = ctx
        .data
        .get("slug")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let page_id = ctx
        .data
        .get("page_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let page = if !slug.is_empty() {
        state.get_page_by_slug(slug)
    } else {
        None
    }
    .or_else(|| {
        if !page_id.is_empty() {
            state.get_page(page_id)
        } else {
            None
        }
    });
    let mut found = false;
    let mut html = String::new();
    let mut title = String::new();
    let mut tagline = String::new();
    let mut description = String::new();
    if let Some(page) = page {
        if page.status == "published" {
            let mut blocks = page.blocks.clone();
            blocks.sort_by_key(|block| block.order);
            for block in &blocks {
                html.push_str(&render_block(state, &page, block));
                html.push('\n');
            }
            if let Some(hero) = blocks.iter().find(|block| block.block_type == "hero") {
                tagline = content_str(&hero.content, "heading").unwrap_or_default();
                description = content_str(&hero.content, "subheading").unwrap_or_default();
            }
            title = page.title.clone();
            found = true;
        }
    }
    if let Some(object) = ctx.data.as_object_mut() {
        object.insert("found".to_string(), Value::Bool(found));
        object.insert("html".to_string(), Value::String(html));
        object.insert("title".to_string(), Value::String(title));
        object.insert("tagline".to_string(), Value::String(tagline));
        object.insert("description".to_string(), Value::String(description));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(block_type: &str, content: Value) -> Block {
        Block { id: "b1".to_string(), block_type: block_type.to_string(), content, order: 0 }
    }

    #[test]
    fn render_hero_escapes_heading_and_renders_cta() {
        let html = render_block_default(&block("hero", json!({
            "heading": "Hello <World>", "subheading": "Sub", "cta_text": "Go", "cta_url": "/go"
        })));
        assert!(html.contains("<section class=\"sb-hero\">"));
        assert!(html.contains("<h1>Hello &lt;World&gt;</h1>"));
        assert!(html.contains("<p class=\"sb-hero-sub\">Sub</p>"));
        assert!(html.contains("<a class=\"sb-hero-cta\" href=\"/go\">Go</a>"));
        assert!(html.ends_with("</section>"));
    }

    #[test]
    fn render_text_passes_admin_html_through_and_escapes_plain_text() {
        let html = render_block_default(&block("text", json!({"html": "<b>bold</b>"})));
        assert_eq!(html, "<div class=\"sb-text\"><b>bold</b></div>");
        let escaped = render_block_default(&block("text", json!({"text": "a < b"})));
        assert_eq!(escaped, "<div class=\"sb-text\"><p>a &lt; b</p></div>");
    }

    #[test]
    fn render_image_includes_alt_and_caption() {
        let html = render_block_default(&block("image", json!({
            "url": "https://cdn.example.com/a.png", "alt": "A pic", "caption": "Cap"
        })));
        assert!(html.contains("<figure class=\"sb-image\">"));
        assert!(html.contains("<img src=\"https://cdn.example.com/a.png\" alt=\"A pic\">"));
        assert!(html.contains("<figcaption>Cap</figcaption>"));
    }

    #[test]
    fn render_gallery_renders_each_image() {
        let html = render_block_default(&block("gallery", json!({"images": [
            {"url": "https://cdn.example.com/1.png", "alt": "One"},
            {"url": "https://cdn.example.com/2.png", "alt": "Two"},
        ]})));
        assert!(html.contains("<div class=\"sb-gallery\">"));
        assert!(html.contains("alt=\"One\""));
        assert!(html.contains("alt=\"Two\""));
    }

    #[test]
    fn render_placeholders_carry_the_referenced_ids() {
        let pricing = render_block_default(&block("pricing_table", json!({"pricing_table_id": "t-1"})));
        assert!(pricing.contains("data-pricing-table-id=\"t-1\""));
        assert!(pricing.contains("/api/v1/pricing-tables/tables/t-1/render"));
        let storefront = render_block_default(&block("marketplace_storefront", json!({"vendor_slug": "acme"})));
        assert!(storefront.contains("data-vendor-slug=\"acme\""));
        assert!(storefront.contains("/api/v1/pricing-tables/marketplace/storefronts/acme"));
        let custom = render_block_default(&block("custom", json!({"html": "<em>raw</em>"})));
        assert_eq!(custom, "<em>raw</em>");
    }

    #[test]
    fn render_unknown_block_type_renders_a_comment() {
        let html = render_block_default(&block("mystery", json!({})));
        assert_eq!(html, "<!-- unknown block type: mystery -->");
    }

    #[test]
    fn render_hero_and_media_reject_dangerous_urls() {
        // `javascript:` survives HTML escaping, so the allowlist must
        // catch it before the URL reaches an attribute.
        let hero = render_block_default(&block("hero", json!({
            "heading": "Hi", "cta_text": "Go", "cta_url": "javascript:alert(1)",
            "image_url": "data:text/html,<script>alert(1)</script>"
        })));
        assert!(hero.contains("<a class=\"sb-hero-cta\" href=\"#\">Go</a>"));
        assert!(!hero.contains("javascript:"));
        assert!(!hero.contains("sb-hero-image"));
        let hero_ok = render_block_default(&block("hero", json!({
            "heading": "Hi", "cta_text": "Go", "cta_url": "https://example.com/go",
            "image_url": "/img/hero.png"
        })));
        assert!(hero_ok.contains("href=\"https://example.com/go\""));
        assert!(hero_ok.contains("src=\"/img/hero.png\""));
        let image = render_block_default(&block("image", json!({
            "url": "JaVaScRiPt:alert(1)", "alt": "X", "caption": "Cap"
        })));
        assert!(!image.contains("<img"));
        assert!(image.contains("<figcaption>Cap</figcaption>"));
        let gallery = render_block_default(&block("gallery", json!({"images": [
            {"url": "https://cdn.example.com/1.png", "alt": "One"},
            {"url": "javascript:alert(1)", "alt": "Bad"},
        ]})));
        assert!(gallery.contains("alt=\"One\""));
        assert!(!gallery.contains("alt=\"Bad\""));
        let form = render_block_default(&block("form", json!({
            "action": "javascript:alert(1)", "fields": []
        })));
        assert!(form.contains("<form class=\"sb-form\" action=\"#\" method=\"post\">"));
        assert!(!form.contains("javascript:"));
    }
}
