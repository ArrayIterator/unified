//! HTTP handlers for taxonomy nodes and tags.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::{HashMap};
use std::sync::{Arc};
use super::helpers::{caller, caller_company, require_tenant, require_permission};
use super::types::{CreateTag, CreateTaxonomyNode, MoveTaxonomyNode, PERM_SOFTWARE_CREATE, PERM_SOFTWARE_MANAGE, PERM_SOFTWARE_READ, TAXONOMY_KINDS, Tag, TagCount, TaxonomyNode, UpdateTag, UpdateTaxonomyNode};
use super::state::SoftwareState;
use super::validation::{build_tree, is_descendant_of, matches_search, new_id, now, paginate, recompute_subtree, respond_created, slugify, taxonomy_path};

// ---------------------------------------------------------------------------
// Taxonomy handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_taxonomy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "slug", "sort_order", "created_at"], "sort_order");
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|node| {
            matches_search(&format!("{} {} {}", node.name, node.slug, node.description), &params)
                && params.filters.get("kind").is_none_or(|kind| node.kind == *kind)
                && params.filters.get("parent_id").is_none_or(|parent_id| {
                    node.parent_id.as_deref() == Some(parent_id.as_str())
                })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        _ => items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.name.cmp(&b.name))
        }),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_CREATE)?;
        let input = body.into_inner();
        if !TAXONOMY_KINDS.contains(&input.kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Kind must be group or category",
            ));
        }
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the taxonomy name",
            ));
        }
        if let Some(parent_id) = &input.parent_id {
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == input.kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent node not found or has a different kind",
                    ))
                }
            }
        }
        if state.taxonomy.iter().any(|entry| {
            entry.value().kind == input.kind
                && entry.value().slug == slug
                && entry.value().parent_id == input.parent_id
        }) {
            return Err(UnifiedError::conflict(format!(
                "A {} with slug \"{slug}\" already exists under this parent",
                input.kind
            )));
        }
        let (depth, path) = taxonomy_path(&state, input.parent_id.as_deref(), &slug);
        let node = TaxonomyNode {
            id: new_id(),
            kind: input.kind,
            slug,
            name: input.name.trim().to_string(),
            description: input.description.unwrap_or_default(),
            parent_id: input.parent_id,
            sort_order: input.sort_order.unwrap_or(0),
            depth,
            path,
            created_at: now(),
            updated_at: now(),
        };
        state.taxonomy.insert(node.id.clone(), node.clone());
        state.emit(
            "software.taxonomy.created",
            serde_json::to_value(&node).unwrap_or(Value::Null),
        );
        Ok(node)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let result: UnifiedResult<TaxonomyNode> = state
        .taxonomy
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"));
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock.
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            let (kind, parent_id) = state
                .taxonomy
                .get(path.as_str())
                .map(|entry| (entry.value().kind.clone(), entry.value().parent_id.clone()))
                .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
            if state.taxonomy.iter().any(|entry| {
                entry.value().kind == kind
                    && entry.value().slug == *slug
                    && entry.value().parent_id == parent_id
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A {kind} with slug \"{slug}\" already exists under this parent"
                )));
            }
        }
        let mut node = state
            .taxonomy
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            node.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            node.slug = slug;
        }
        if let Some(description) = input.description {
            node.description = description;
        }
        if let Some(sort_order) = input.sort_order {
            node.sort_order = sort_order;
        }
        node.updated_at = now();
        let updated = node.clone();
        drop(node);
        recompute_subtree(&state, path.as_str());
        let updated = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .unwrap_or(updated);
        state.emit(
            "software.taxonomy.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let node = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if state
            .taxonomy
            .iter()
            .any(|entry| entry.value().parent_id.as_deref() == Some(path.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Taxonomy node has children; move or delete the children first",
            ));
        }
        let referenced = state.software.iter().any(|entry| {
            (node.kind == "group" && entry.value().group_id.as_deref() == Some(path.as_str()))
                || (node.kind == "category"
                    && entry.value().category_id.as_deref() == Some(path.as_str()))
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Taxonomy node is referenced by software records",
            ));
        }
        state.taxonomy.remove(path.as_str());
        state.emit(
            "software.taxonomy.deleted",
            serde_json::to_value(&node).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn move_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MoveTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let input = body.into_inner();
        let kind = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().kind.clone())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if let Some(parent_id) = &input.parent_id {
            if parent_id == path.as_str() {
                return Err(UnifiedError::bad_request(
                    "A node cannot be its own parent",
                ));
            }
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent node not found or has a different kind",
                    ))
                }
            }
            if is_descendant_of(&state, parent_id, path.as_str()) {
                return Err(UnifiedError::bad_request(
                    "A node cannot be moved under one of its own descendants",
                ));
            }
        }
        let mut node = state
            .taxonomy
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        node.parent_id = input.parent_id;
        if let Some(sort_order) = input.sort_order {
            node.sort_order = sort_order;
        }
        node.updated_at = now();
        drop(node);
        recompute_subtree(&state, path.as_str());
        let moved = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("node still present");
        state.emit(
            "software.taxonomy.updated",
            serde_json::to_value(&moved).unwrap_or(Value::Null),
        );
        Ok(moved)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_taxonomy_children(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    if !state.taxonomy.contains_key(path.as_str()) {
        return json::error_response(
            &UnifiedError::not_found("Taxonomy node not found"),
            ctx.is_debug(),
        );
    }
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| entry.value().parent_id.as_deref() == Some(path.as_str()))
        .map(|entry| entry.value().clone())
        .filter(|node| {
            matches_search(&format!("{} {} {}", node.name, node.slug, node.description), &params)
        })
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn taxonomy_tree(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let kind = params.filters.get("kind").cloned();
    if let Some(kind) = &kind {
        if !TAXONOMY_KINDS.contains(&kind.as_str()) {
            return json::error_response(
                &UnifiedError::bad_request("Kind must be group or category"),
                false,
            );
        }
    }
    let roots = build_tree(&state, None, kind.as_deref());
    let page = paginate(roots, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Tag handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_tags(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "slug", "created_at"], "created_at");
    let mut items: Vec<Tag> = state
        .tags
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tag| matches_search(&format!("{} {}", tag.name, tag.slug), &params))
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    body: web::Json<CreateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_CREATE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the tag name",
            ));
        }
        if state.tags.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "Tag with slug \"{slug}\" already exists"
            )));
        }
        let tag = Tag {
            id: new_id(),
            slug,
            name: input.name.trim().to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.tags.insert(tag.id.clone(), tag.clone());
        state.emit(
            "software.taxonomy.created",
            serde_json::to_value(&tag).unwrap_or(Value::Null),
        );
        Ok(tag)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let result: UnifiedResult<Tag> = state
        .tags
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Tag not found"));
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock.
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state
                .tags
                .iter()
                .any(|entry| entry.value().slug == *slug && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Tag with slug \"{slug}\" already exists"
                )));
            }
        }
        let mut tag = state
            .tags
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tag not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            tag.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            tag.slug = slug;
        }
        tag.updated_at = now();
        let updated = tag.clone();
        drop(tag);
        state.emit(
            "software.taxonomy.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let tag = state
            .tags
            .remove(path.as_str())
            .map(|(_, tag)| tag)
            .ok_or_else(|| UnifiedError::not_found("Tag not found"))?;
        for mut entry in state.software_tags.iter_mut() {
            entry.value_mut().remove(path.as_str());
        }
        state.emit(
            "software.taxonomy.deleted",
            serde_json::to_value(&tag).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn tag_cloud(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "software_count", "created_at"], "software_count");
    let mut counts: HashMap<String, u64> = HashMap::new();
    for entry in state.software_tags.iter() {
        for tag_id in entry.value().iter() {
            *counts.entry(tag_id.clone()).or_default() += 1;
        }
    }
    let mut items: Vec<TagCount> = state
        .tags
        .iter()
        .map(|entry| TagCount {
            tag: entry.value().clone(),
            software_count: counts.get(entry.key()).copied().unwrap_or(0),
        })
        .filter(|item| matches_search(&format!("{} {}", item.tag.name, item.tag.slug), &params))
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.tag.name.cmp(&b.tag.name)),
        "created_at" => items.sort_by(|a, b| a.tag.created_at.cmp(&b.tag.created_at)),
        _ => items.sort_by(|a, b| {
            b.software_count
                .cmp(&a.software_count)
                .then_with(|| a.tag.name.cmp(&b.tag.name))
        }),
    }
    if params.normalized().order == "asc" && sort == "software_count" {
        items.reverse();
    } else if params.normalized().order == "desc" && sort != "software_count" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}
