//! Knowledgebase - HTTP handlers for categories.

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
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_articles::*, handlers_tickets::*};

// ---------------------------------------------------------------------------
// Category handlers
// ---------------------------------------------------------------------------

pub(crate) async fn create_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Category> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_MANAGE)?;
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

pub(crate) async fn list_categories(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Category>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
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

pub(crate) async fn get_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Category> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
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

pub(crate) async fn update_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Category> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_MANAGE)?;
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

pub(crate) async fn delete_category(
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
