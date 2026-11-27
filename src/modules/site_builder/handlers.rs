// Site Builder - HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
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
use super::rendering::*;
use super::module::*;

// Inputs

#[derive(Debug, Deserialize)]
pub(crate) struct PageInput {
    title: String,
    slug: Option<String>,
    theme_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PageUpdateInput {
    title: Option<String>,
    slug: Option<String>,
    theme_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BlockInput {
    #[serde(rename = "type")]
    block_type: String,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BlockUpdateInput {
    #[serde(rename = "type")]
    block_type: Option<String>,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SectionInput {
    name: String,
    layout: Option<String>,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SectionUpdateInput {
    name: Option<String>,
    layout: Option<String>,
    content: Option<Value>,
    order: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OrderInput {
    ids: Vec<String>,
}

// Page handlers

pub(crate) async fn list_pages(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn create_page(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    body: web::Json<PageInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn get_page(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let debug = state.is_debug();
    match state.get_page(&path.into_inner()) {
        Some(page) => api_json::success_response(page, 200),
        None => not_found_page(debug),
    }
}

pub(crate) async fn update_page(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PageUpdateInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn delete_page(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

// Block handlers

/// Blocks of one page, paginated with the standard
/// `PaginationParams` / `PageResult` (blocks are ordered by their
/// `order` field before slicing).
pub(crate) async fn list_blocks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let debug = state.is_debug();
    match state.get_page(&path.into_inner()) {
        Some(mut page) => {
            page.blocks.sort_by_key(|block| block.order);
            let pagination = query.normalized();
            let total = page.blocks.len() as u64;
            let start = ((pagination.page - 1) * pagination.per_page) as usize;
            let page_items: Vec<Block> = page
                .blocks
                .into_iter()
                .skip(start)
                .take(pagination.per_page as usize)
                .collect();
            api_json::success_response(
                PageResult::new(page_items, total, &pagination).to_json(),
                200,
            )
        }
        None => not_found_page(debug),
    }
}

pub(crate) async fn create_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<BlockInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let debug = state.is_debug();
    let id = path.into_inner();
    let input = body.into_inner();
    let known = BLOCK_TYPES;
    if !known.contains(&input.block_type.as_str()) {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Block type must be one of: {}",
                known.join(", ")
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

pub(crate) async fn update_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<BlockUpdateInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn delete_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn reorder_blocks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<OrderInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

// Section handlers

/// Sections of one page, paginated with the standard
/// `PaginationParams` / `PageResult` (sections are ordered by their
/// `order` field before slicing).
pub(crate) async fn list_sections(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let debug = state.is_debug();
    match state.get_page(&path.into_inner()) {
        Some(mut page) => {
            page.sections.sort_by_key(|section| section.order);
            let pagination = query.normalized();
            let total = page.sections.len() as u64;
            let start = ((pagination.page - 1) * pagination.per_page) as usize;
            let page_items: Vec<Section> = page
                .sections
                .into_iter()
                .skip(start)
                .take(pagination.per_page as usize)
                .collect();
            api_json::success_response(
                PageResult::new(page_items, total, &pagination).to_json(),
                200,
            )
        }
        None => not_found_page(debug),
    }
}

pub(crate) async fn create_section(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SectionInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn update_section(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<SectionUpdateInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn delete_section(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

pub(crate) async fn reorder_sections(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<OrderInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
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

// Publish flow and rendering

pub(crate) async fn publish_page(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let state: Arc<SiteBuilderState> = state.get_ref().clone();
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

pub(crate) async fn unpublish_page(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let state: Arc<SiteBuilderState> = state.get_ref().clone();
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

/// PUBLIC: render a published page to HTML. No auth is required;
/// public access is gated by the API Manager's `ApiAccessRule` for
/// this path (default deny without a rule). Drafts are never served
/// here: a draft renders only as a preview for a caller holding the
/// manage capability, and anyone else gets the same 404 as a missing
/// page so unpublished content does not leak.
pub(crate) async fn render_page(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SiteBuilderState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SiteBuilderState> = state.get_ref().clone();
    let debug = state.is_debug();
    let mut page = match state.get_page(&path.into_inner()) {
        Some(page) => page,
        None => return not_found_page(debug),
    };
    // A tenant without the site-builder package gets a uniform 404
    // here, never a confirmation that the page exists.
    if let Err(error) = require_site_builder_entitlement(&ctx, &req) {
        let _ = error;
        return not_found_page(debug);
    }
    if !render_visible(&page.status, authorize(&ctx, &req, PERM_SITE_BUILDER_MANAGE).is_ok()) {
        return not_found_page(debug);
    }
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

/// True when a page may be rendered: published pages are public;
/// drafts need a preview (the caller holds the manage capability).
pub(crate) fn render_visible(status: &str, preview_allowed: bool) -> bool {
    status == "published" || preview_allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_visible_serves_published_only_without_preview() {
        assert!(render_visible("published", false));
        assert!(render_visible("published", true));
        assert!(render_visible("draft", true));
        assert!(!render_visible("draft", false));
        assert!(!render_visible("", false));
    }
}
