// Site Builder module.
//
// Drag-and-drop page building on top of Content and Theme: pages made of
// ordered blocks and sections, rendered to real HTML server-side, with a
// draft -> published flow.
//
// Routes (all under `/api/v1/site-builder`):
//   GET    /pages                                   - paginated pages
//   POST   /pages                                   - create a page {title, slug, theme_id}
//   GET    /pages/{id}                              - one page
//   PUT    /pages/{id}                              - update a page
//   DELETE /pages/{id}                              - delete a page
//   GET    /pages/{id}/blocks                       - list blocks (in order)
//   POST   /pages/{id}/blocks                       - add a block
//   PUT    /pages/{id}/blocks/{block_id}            - update a block
//   DELETE /pages/{id}/blocks/{block_id}            - delete a block
//   PUT    /pages/{id}/blocks/order                 - reorder blocks {ids: [...]}
//   GET    /pages/{id}/sections                     - list sections (in order)
//   POST   /pages/{id}/sections                     - add a section
//   PUT    /pages/{id}/sections/{section_id}        - update a section
//   DELETE /pages/{id}/sections/{section_id}        - delete a section
//   PUT    /pages/{id}/sections/order               - reorder sections {ids: [...]}
//   POST   /pages/{id}/publish                      - draft -> published
//   POST   /pages/{id}/unpublish                    - published -> draft
//   GET    /pages/{id}/render                       - render the page to HTML
//
// Hook contracts:
//   Publish validates the theme through the `theme.exists` event owned by
//   the Theme module. When no handler answers, the page is published with
//   a warning in the response (the theme is accepted unchecked).
//   Rendering goes through the theme engine contract: every block is
//   rendered to a default HTML fragment and then offered to the Theme
//   module through the `theme.render_block` event, which may replace the
//   fragment. Emits `site.page_published` / `site.page_unpublished`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
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

pub const EVENT_THEME_EXISTS: &str = "theme.exists";
pub const EVENT_THEME_RENDER_BLOCK: &str = "theme.render_block";
pub const EVENT_PAGE_PUBLISHED: &str = "site.page_published";
pub const EVENT_PAGE_UNPUBLISHED: &str = "site.page_unpublished";

/// Block types the server-side renderer knows how to render.
pub const BLOCK_TYPES: &[&str] = &["hero", "text", "image", "gallery", "form", "custom"];
/// Section layouts.
pub const SECTION_LAYOUTS: &[&str] = &[
    "full",
    "two-column",
    "three-column",
    "sidebar-left",
    "sidebar-right",
];

fn slug_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").expect("slug regex must compile")
    })
}

/// Escape text for safe inclusion in HTML content and attributes.
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Derive a URL slug from a title (`"Hello World!"` -> `"hello-world"`).
fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = true;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
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

#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub id: String,
    #[serde(rename = "type")]
    pub block_type: String,
    pub content: Value,
    pub order: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub id: String,
    pub name: String,
    pub layout: String,
    pub content: Value,
    pub order: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub theme_id: String,
    /// `draft` or `published`.
    pub status: String,
    pub blocks: Vec<Block>,
    pub sections: Vec<Section>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct SiteBuilderState {
    pages: RwLock<HashMap<String, Page>>,
    hooks: RwLock<Option<Arc<HookBus>>>,
    debug: AtomicBool,
}

impl SiteBuilderState {
    fn new() -> Self {
        Self {
            pages: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("site builder hooks lock poisoned") = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("site builder hooks lock poisoned").clone()
    }

    fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    fn get_page(&self, id: &str) -> Option<Page> {
        self.pages
            .read()
            .expect("site builder lock poisoned")
            .get(id)
            .cloned()
    }

    fn slug_taken(&self, slug: &str, except_id: Option<&str>) -> bool {
        self.pages
            .read()
            .expect("site builder lock poisoned")
            .values()
            .any(|page| page.slug == slug && Some(page.id.as_str()) != except_id)
    }

    /// Ask the Theme module whether a theme exists. Returns `Ok(true)` /
    /// `Ok(false)` when the Theme module answers, and `Ok(None)` when no
    /// handler is registered for `theme.exists`.
    fn theme_exists(&self, theme_id: &str) -> Option<bool> {
        let hooks = self.hooks()?;
        let answer = hooks.emit(EVENT_THEME_EXISTS, json!({ "theme_id": theme_id }));
        answer.results.first().and_then(|result| {
            result
                .get("exists")
                .and_then(|v| v.as_bool())
                .or_else(|| result.get("found").and_then(|v| v.as_bool()))
        })
    }
}

// ---------------------------------------------------------------------------
// Server-side block rendering (theme engine contract)
// ---------------------------------------------------------------------------

fn content_str(content: &Value, key: &str) -> Option<String> {
    content
        .get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

fn content_bool(content: &Value, key: &str) -> bool {
    content.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Render one block to its default HTML fragment. The Theme module may
/// replace the fragment through the `theme.render_block` event (see
/// `render_block`).
fn render_block_default(block: &Block) -> String {
    let content = &block.content;
    match block.block_type.as_str() {
        "hero" => {
            let heading = content_str(content, "heading").unwrap_or_default();
            let subheading = content_str(content, "subheading").unwrap_or_default();
            let image_url = content_str(content, "image_url").unwrap_or_default();
            let cta_text = content_str(content, "cta_text").unwrap_or_default();
            let cta_url = content_str(content, "cta_url").unwrap_or_default();
            let mut html = String::from("<section class=\"sb-hero\">");
            if !image_url.is_empty() {
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
                let url = if cta_url.is_empty() { "#".to_string() } else { cta_url };
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
            let mut html = format!(
                "<figure class=\"sb-image\"><img src=\"{}\" alt=\"{}\">",
                escape_html(&url),
                escape_html(&alt)
            );
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
                    html.push_str(&format!(
                        "<img src=\"{}\" alt=\"{}\">",
                        escape_html(&url),
                        escape_html(&alt)
                    ));
                }
            }
            html.push_str("</div>");
            html
        }
        "form" => {
            let action = content_str(content, "action").unwrap_or_else(|| "#".to_string());
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
        other => format!("<!-- unknown block type: {} -->", escape_html(other)),
    }
}

/// Render one block, letting the Theme module replace the fragment through
/// the `theme.render_block` event.
fn render_block(state: &SiteBuilderState, page: &Page, block: &Block) -> String {
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

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PageInput {
    title: String,
    slug: Option<String>,
    theme_id: String,
}

#[derive(Debug, Deserialize)]
struct PageUpdateInput {
    title: Option<String>,
    slug: Option<String>,
    theme_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlockInput {
    #[serde(rename = "type")]
    block_type: String,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct BlockUpdateInput {
    #[serde(rename = "type")]
    block_type: Option<String>,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SectionInput {
    name: String,
    layout: Option<String>,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SectionUpdateInput {
    name: Option<String>,
    layout: Option<String>,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OrderInput {
    ids: Vec<String>,
}

fn validate_slug(slug: &str) -> UnifiedResult<()> {
    if !slug_regex().is_match(slug) {
        return Err(UnifiedError::bad_request(
            "Slug must be lowercase letters, digits, and single hyphens (for example 'about-us')",
        ));
    }
    Ok(())
}

fn not_found_page(debug: bool) -> HttpResponse {
    api_json::error_response(&UnifiedError::not_found("Page not found"), debug)
}

// ---------------------------------------------------------------------------
// Page handlers
// ---------------------------------------------------------------------------

async fn list_pages(
    state: web::Data<Arc<SiteBuilderState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let mut pages: Vec<Page> = state
        .pages
        .read()
        .expect("site builder lock poisoned")
        .values()
        .cloned()
        .collect();
    if let Some(status) = pagination.filters.get("status") {
        pages.retain(|page| page.status == *status);
    }
    if let Some(theme_id) = pagination.filters.get("theme_id") {
        pages.retain(|page| page.theme_id == *theme_id);
    }
    if let Some(search) = &pagination.search {
        let needle = search.to_lowercase();
        pages.retain(|page| {
            page.title.to_lowercase().contains(&needle)
                || page.slug.to_lowercase().contains(&needle)
        });
    }
    let sort = query.allowed_sort(&["title", "slug", "status", "created_at", "updated_at"], "created_at");
    match sort.as_str() {
        "title" => pages.sort_by(|a, b| a.title.cmp(&b.title)),
        "slug" => pages.sort_by(|a, b| a.slug.cmp(&b.slug)),
        "status" => pages.sort_by(|a, b| a.status.cmp(&b.status)),
        "updated_at" => pages.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        _ => pages.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if pagination.order == "desc" {
        pages.reverse();
    }
    let total = pages.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<Page> = pages
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn create_page(
    state: web::Data<Arc<SiteBuilderState>>,
    body: web::Json<PageInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if input.title.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Page title must not be empty"),
            debug,
        );
    }
    if input.theme_id.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("theme_id must not be empty"),
            debug,
        );
    }
    let slug = match input.slug {
        Some(slug) if !slug.trim().is_empty() => slug.trim().to_lowercase(),
        _ => slugify(&input.title),
    };
    if slug.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Could not derive a slug from the title; provide one explicitly"),
            debug,
        );
    }
    if let Err(error) = validate_slug(&slug) {
        return api_json::error_response(&error, debug);
    }
    if state.slug_taken(&slug, None) {
        return api_json::error_response(
            &UnifiedError::conflict(format!("A page with slug '{slug}' already exists")),
            debug,
        );
    }
    let now = Utc::now().to_rfc3339();
    let page = Page {
        id: Uuid::now_v7().to_string(),
        title: input.title.trim().to_string(),
        slug,
        theme_id: input.theme_id.trim().to_string(),
        status: "draft".to_string(),
        blocks: Vec::new(),
        sections: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    {
        let mut pages = state.pages.write().expect("site builder lock poisoned");
        pages.insert(page.id.clone(), page.clone());
    }
    api_json::success_response(page, 201)
}

async fn get_page(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get_page(&path.into_inner()) {
        Some(page) => api_json::success_response(page, 200),
        None => not_found_page(debug),
    }
}

async fn update_page(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
    body: web::Json<PageUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = path.into_inner();
    let input = body.into_inner();
    if let Some(title) = &input.title {
        if title.trim().is_empty() {
            return api_json::error_response(
                &UnifiedError::bad_request("Page title must not be empty"),
                debug,
            );
        }
    }
    if let Some(slug) = &input.slug {
        if let Err(error) = validate_slug(slug.trim()) {
            return api_json::error_response(&error, debug);
        }
        if state.slug_taken(slug.trim(), Some(&id)) {
            return api_json::error_response(
                &UnifiedError::conflict(format!("A page with slug '{slug}' already exists")),
                debug,
            );
        }
    }
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    if let Some(title) = input.title {
        page.title = title.trim().to_string();
    }
    if let Some(slug) = input.slug {
        page.slug = slug.trim().to_lowercase();
    }
    if let Some(theme_id) = input.theme_id {
        if theme_id.trim().is_empty() {
            return api_json::error_response(
                &UnifiedError::bad_request("theme_id must not be empty"),
                debug,
            );
        }
        page.theme_id = theme_id.trim().to_string();
    }
    page.updated_at = Utc::now().to_rfc3339();
    let page = page.clone();
    api_json::success_response(page, 200)
}

async fn delete_page(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let removed = state
        .pages
        .write()
        .expect("site builder lock poisoned")
        .remove(&path.into_inner());
    match removed {
        Some(page) => {
            api_json::success_response(json!({ "deleted": true, "id": page.id }), 200)
        }
        None => not_found_page(debug),
    }
}

// ---------------------------------------------------------------------------
// Block handlers
// ---------------------------------------------------------------------------

async fn list_blocks(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get_page(&path.into_inner()) {
        Some(mut page) => {
            page.blocks.sort_by_key(|block| block.order);
            api_json::success_response(page.blocks, 200)
        }
        None => not_found_page(debug),
    }
}

async fn create_block(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
    body: web::Json<BlockInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = path.into_inner();
    let input = body.into_inner();
    if !BLOCK_TYPES.contains(&input.block_type.as_str()) {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Block type must be one of: {}",
                BLOCK_TYPES.join(", ")
            )),
            debug,
        );
    }
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    let order = input.order.unwrap_or_else(|| {
        page.blocks.iter().map(|block| block.order).max().map(|max| max + 1).unwrap_or(0)
    });
    let block = Block {
        id: Uuid::now_v7().to_string(),
        block_type: input.block_type,
        content: input.content.unwrap_or(Value::Null),
        order,
    };
    page.blocks.push(block.clone());
    page.blocks.sort_by_key(|block| block.order);
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(block, 201)
}

async fn update_block(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<(String, String)>,
    body: web::Json<BlockUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let (page_id, block_id) = path.into_inner();
    let input = body.into_inner();
    if let Some(block_type) = &input.block_type {
        if !BLOCK_TYPES.contains(&block_type.as_str()) {
            return api_json::error_response(
                &UnifiedError::bad_request(format!(
                    "Block type must be one of: {}",
                    BLOCK_TYPES.join(", ")
                )),
                debug,
            );
        }
    }
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&page_id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    let block = match page.blocks.iter_mut().find(|block| block.id == block_id) {
        Some(block) => block,
        None => {
            return api_json::error_response(&UnifiedError::not_found("Block not found"), debug)
        }
    };
    if let Some(block_type) = input.block_type {
        block.block_type = block_type;
    }
    if let Some(content) = input.content {
        block.content = content;
    }
    if let Some(order) = input.order {
        block.order = order;
    }
    let block = block.clone();
    page.blocks.sort_by_key(|block| block.order);
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(block, 200)
}

async fn delete_block(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = state.is_debug();
    let (page_id, block_id) = path.into_inner();
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&page_id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    let before = page.blocks.len();
    page.blocks.retain(|block| block.id != block_id);
    if page.blocks.len() == before {
        return api_json::error_response(&UnifiedError::not_found("Block not found"), debug);
    }
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(json!({ "deleted": true, "id": block_id }), 200)
}

async fn reorder_blocks(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
    body: web::Json<OrderInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let page_id = path.into_inner();
    let ids = body.into_inner().ids;
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&page_id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    if ids.len() != page.blocks.len()
        || !ids.iter().all(|id| page.blocks.iter().any(|block| &block.id == id))
    {
        return api_json::error_response(
            &UnifiedError::bad_request(
                "ids must contain exactly the current block ids, in the new order",
            ),
            debug,
        );
    }
    let mut reordered: Vec<Block> = Vec::with_capacity(ids.len());
    for (order, id) in ids.iter().enumerate() {
        let mut block = page
            .blocks
            .iter()
            .find(|block| &block.id == id)
            .expect("block id validated")
            .clone();
        block.order = order as u32;
        reordered.push(block);
    }
    page.blocks = reordered;
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(page.blocks.clone(), 200)
}

// ---------------------------------------------------------------------------
// Section handlers
// ---------------------------------------------------------------------------

async fn list_sections(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get_page(&path.into_inner()) {
        Some(mut page) => {
            page.sections.sort_by_key(|section| section.order);
            api_json::success_response(page.sections, 200)
        }
        None => not_found_page(debug),
    }
}

async fn create_section(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
    body: web::Json<SectionInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = path.into_inner();
    let input = body.into_inner();
    if input.name.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Section name must not be empty"),
            debug,
        );
    }
    let layout = input.layout.unwrap_or_else(|| "full".to_string());
    if !SECTION_LAYOUTS.contains(&layout.as_str()) {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Section layout must be one of: {}",
                SECTION_LAYOUTS.join(", ")
            )),
            debug,
        );
    }
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    let order = input.order.unwrap_or_else(|| {
        page.sections.iter().map(|section| section.order).max().map(|max| max + 1).unwrap_or(0)
    });
    let section = Section {
        id: Uuid::now_v7().to_string(),
        name: input.name.trim().to_string(),
        layout,
        content: input.content.unwrap_or(Value::Null),
        order,
    };
    page.sections.push(section.clone());
    page.sections.sort_by_key(|section| section.order);
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(section, 201)
}

async fn update_section(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<(String, String)>,
    body: web::Json<SectionUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let (page_id, section_id) = path.into_inner();
    let input = body.into_inner();
    if let Some(layout) = &input.layout {
        if !SECTION_LAYOUTS.contains(&layout.as_str()) {
            return api_json::error_response(
                &UnifiedError::bad_request(format!(
                    "Section layout must be one of: {}",
                    SECTION_LAYOUTS.join(", ")
                )),
                debug,
            );
        }
    }
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&page_id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    let section = match page.sections.iter_mut().find(|section| section.id == section_id) {
        Some(section) => section,
        None => {
            return api_json::error_response(&UnifiedError::not_found("Section not found"), debug)
        }
    };
    if let Some(name) = input.name {
        section.name = name.trim().to_string();
    }
    if let Some(layout) = input.layout {
        section.layout = layout;
    }
    if let Some(content) = input.content {
        section.content = content;
    }
    if let Some(order) = input.order {
        section.order = order;
    }
    let section = section.clone();
    page.sections.sort_by_key(|section| section.order);
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(section, 200)
}

async fn delete_section(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = state.is_debug();
    let (page_id, section_id) = path.into_inner();
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&page_id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    let before = page.sections.len();
    page.sections.retain(|section| section.id != section_id);
    if page.sections.len() == before {
        return api_json::error_response(&UnifiedError::not_found("Section not found"), debug);
    }
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(json!({ "deleted": true, "id": section_id }), 200)
}

async fn reorder_sections(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
    body: web::Json<OrderInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let page_id = path.into_inner();
    let ids = body.into_inner().ids;
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&page_id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    if ids.len() != page.sections.len()
        || !ids.iter().all(|id| page.sections.iter().any(|section| &section.id == id))
    {
        return api_json::error_response(
            &UnifiedError::bad_request(
                "ids must contain exactly the current section ids, in the new order",
            ),
            debug,
        );
    }
    let mut reordered: Vec<Section> = Vec::with_capacity(ids.len());
    for (order, id) in ids.iter().enumerate() {
        let mut section = page
            .sections
            .iter()
            .find(|section| &section.id == id)
            .expect("section id validated")
            .clone();
        section.order = order as u32;
        reordered.push(section);
    }
    page.sections = reordered;
    page.updated_at = Utc::now().to_rfc3339();
    api_json::success_response(page.sections.clone(), 200)
}

// ---------------------------------------------------------------------------
// Publish flow and rendering
// ---------------------------------------------------------------------------

async fn publish_page(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SiteBuilderState> = state.into_inner();
    let debug = state.is_debug();
    let id = path.into_inner();
    let page = match state.get_page(&id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    if page.blocks.is_empty() {
        return api_json::error_response(
            &UnifiedError::unprocessable("Cannot publish a page without at least one block"),
            debug,
        );
    }
    let mut warnings: Vec<String> = Vec::new();
    match state.theme_exists(&page.theme_id) {
        Some(true) => {}
        Some(false) => {
            return api_json::error_response(
                &UnifiedError::unprocessable(format!(
                    "Cannot publish: theme '{}' does not exist",
                    page.theme_id
                )),
                debug,
            );
        }
        None => warnings.push(format!(
            "Theme '{}' could not be verified: no theme handler answered the theme.exists event; published unchecked",
            page.theme_id
        )),
    }
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    page.status = "published".to_string();
    page.updated_at = Utc::now().to_rfc3339();
    let page = page.clone();
    drop(pages);
    if let Some(hooks) = state.hooks() {
        hooks.emit(
            EVENT_PAGE_PUBLISHED,
            json!({ "page_id": page.id, "slug": page.slug, "theme_id": page.theme_id }),
        );
    }
    let mut data = serde_json::to_value(&page).expect("page serializes");
    if !warnings.is_empty() {
        data.as_object_mut()
            .expect("page value is an object")
            .insert("warnings".to_string(), json!(warnings));
    }
    api_json::success_response(data, 200)
}

async fn unpublish_page(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SiteBuilderState> = state.into_inner();
    let debug = state.is_debug();
    let id = path.into_inner();
    let mut pages = state.pages.write().expect("site builder lock poisoned");
    let page = match pages.get_mut(&id) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    page.status = "draft".to_string();
    page.updated_at = Utc::now().to_rfc3339();
    let page = page.clone();
    drop(pages);
    if let Some(hooks) = state.hooks() {
        hooks.emit(
            EVENT_PAGE_UNPUBLISHED,
            json!({ "page_id": page.id, "slug": page.slug }),
        );
    }
    api_json::success_response(page, 200)
}

async fn render_page(
    state: web::Data<Arc<SiteBuilderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SiteBuilderState> = state.into_inner();
    let debug = state.is_debug();
    let mut page = match state.get_page(&path.into_inner()) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    page.blocks.sort_by_key(|block| block.order);
    let mut body = String::new();
    for block in &page.blocks {
        body.push_str(&render_block(&state, &page, block));
        body.push('\n');
    }
    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n</head>\n<body>\n<main class=\"sb-page\" data-page-id=\"{}\" data-theme-id=\"{}\">\n{}</main>\n</body>\n</html>",
        escape_html(&page.title),
        page.id,
        escape_html(&page.theme_id),
        body
    );
    api_json::success_response(
        json!({
            "page_id": page.id,
            "title": page.title,
            "slug": page.slug,
            "status": page.status,
            "theme_id": page.theme_id,
            "html": html
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SiteBuilderModule {
    state: Arc<SiteBuilderState>,
}

impl SiteBuilderModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SiteBuilderState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SiteBuilderModule::new()));
}

impl Module for SiteBuilderModule {
    fn id(&self) -> &'static str {
        "site-builder"
    }

    fn name(&self) -> &'static str {
        "Site Builder"
    }

    fn description(&self) -> &'static str {
        "Drag-and-drop page building on top of Content and Theme: pages of ordered blocks and sections, server-side HTML rendering, and a draft to published flow."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["content", "theme"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {}

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/site-builder")
                .app_data(web::Data::new(state))
                .route("/pages", web::get().to(list_pages))
                .route("/pages", web::post().to(create_page))
                .route("/pages/{id}", web::get().to(get_page))
                .route("/pages/{id}", web::put().to(update_page))
                .route("/pages/{id}", web::delete().to(delete_page))
                .route("/pages/{id}/blocks", web::get().to(list_blocks))
                .route("/pages/{id}/blocks", web::post().to(create_block))
                .route("/pages/{id}/blocks/order", web::put().to(reorder_blocks))
                .route("/pages/{id}/blocks/{block_id}", web::put().to(update_block))
                .route("/pages/{id}/blocks/{block_id}", web::delete().to(delete_block))
                .route("/pages/{id}/sections", web::get().to(list_sections))
                .route("/pages/{id}/sections", web::post().to(create_section))
                .route("/pages/{id}/sections/order", web::put().to(reorder_sections))
                .route("/pages/{id}/sections/{section_id}", web::put().to(update_section))
                .route("/pages/{id}/sections/{section_id}", web::delete().to(delete_section))
                .route("/pages/{id}/publish", web::post().to(publish_page))
                .route("/pages/{id}/unpublish", web::post().to(unpublish_page))
                .route("/pages/{id}/render", web::get().to(render_page)),
        );
    }
}
