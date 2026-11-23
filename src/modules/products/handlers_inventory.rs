//! HTTP handlers for inventory, files, versions, taxonomy and tags.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use semver::Version;
use serde_json::{json, Value};
use std::collections::{HashMap};
use std::sync::{Arc};
use super::helpers::{
    caller, caller_company, in_company_scope, require_permission, require_product_in_scope,
    require_tenant,
};
use super::types::{AdjustInventory, AttachTag, CreateProductFile, CreateProductVersion, CreateTag, CreateTaxonomyNode, Inventory, MoveTaxonomyNode, PERM_PRODUCTS_CREATE, PERM_PRODUCTS_MANAGE, PERM_PRODUCTS_READ, ProductFile, ProductVersion, TAXONOMY_KINDS, Tag, TagCount, TaxonomyNode, UpdateTag, UpdateTaxonomyNode};
use super::state::ProductsState;
use super::validation::{build_tree, is_descendant_of, matches_search, new_id, now, paginate, recompute_subtree, respond_created, slugify, taxonomy_path, validate_hex_checksum};

// ---------------------------------------------------------------------------
// Inventory handlers (physical products only)
// ---------------------------------------------------------------------------

pub(crate) async fn list_inventory(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let _product = match require_product_in_scope(&state, path.as_str(), &_scope) {
        Ok(product) => product,
        Err(error) => {
            return json::respond(Err::<Value, UnifiedError>(error), false);
        }
    };
    let items: Vec<Inventory> = state
        .inventory
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn list_low_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<Inventory> = state
        .inventory
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            record.quantity - record.reserved <= record.low_stock_threshold
                && state
                    .products
                    .get(&record.product_id)
                    .map(|entry| in_company_scope(&entry.value().vendor_company_id, &_scope))
                    .unwrap_or(false)
                && params.filters.get("product_id").is_none_or(|product_id| {
                    record.product_id == *product_id
                })
        })
        .collect();
    items.sort_by(|a, b| {
        (a.quantity - a.reserved).cmp(&(b.quantity - b.reserved))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn adjust_inventory(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AdjustInventory>,
) -> HttpResponse {
    let result: UnifiedResult<Inventory> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let product_id = path.into_inner();
        let product = require_product_in_scope(&state, &product_id, &_scope)?;
        if product.kind != "physical" {
            return Err(UnifiedError::bad_request(
                "Inventory only applies to physical products",
            ));
        }
        if let Some(variant_id) = &input.variant_id {
            match state.variants.get(variant_id.as_str()) {
                Some(variant) if variant.product_id == product_id => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Variant not found for this product",
                    ));
                }
            }
        }
        let quantity_delta = input.quantity_delta.unwrap_or(0);
        let reserved_delta = input.reserved_delta.unwrap_or(0);
        if let Some(threshold) = input.low_stock_threshold {
            if threshold < 0 {
                return Err(UnifiedError::bad_request(
                    "Low-stock threshold must not be negative",
                ));
            }
        }
        // Find the existing record for this product / variant pair, or
        // create one (a fresh record starts at zero stock).
        let record_id = state
            .inventory
            .iter()
            .find(|entry| {
                entry.value().product_id == product_id
                    && entry.value().variant_id == input.variant_id
            })
            .map(|entry| entry.key().clone());
        let mut record = match record_id {
            Some(id) => state
                .inventory
                .get_mut(&id)
                .expect("inventory record still present")
                .clone(),
            None => Inventory {
                id: new_id(),
                product_id: product_id.clone(),
                variant_id: input.variant_id.clone(),
                quantity: 0,
                reserved: 0,
                low_stock_threshold: input.low_stock_threshold.unwrap_or(0),
                created_at: now(),
                updated_at: now(),
            },
        };
        let new_quantity = record
            .quantity
            .checked_add(quantity_delta)
            .ok_or_else(|| UnifiedError::bad_request("Stock quantity overflows"))?;
        if new_quantity < 0 {
            return Err(UnifiedError::bad_request(
                "Stock quantity must not go below zero",
            ));
        }
        let new_reserved = record
            .reserved
            .checked_add(reserved_delta)
            .ok_or_else(|| UnifiedError::bad_request("Reserved stock overflows"))?;
        if new_reserved < 0 || new_reserved > new_quantity {
            return Err(UnifiedError::bad_request(
                "Reserved stock must stay between zero and the on-hand quantity",
            ));
        }
        record.quantity = new_quantity;
        record.reserved = new_reserved;
        if let Some(threshold) = input.low_stock_threshold {
            record.low_stock_threshold = threshold;
        }
        record.updated_at = now();
        let updated = record.clone();
        state.inventory.insert(record.id.clone(), record);
        state.emit(
            "product.inventory.adjusted",
            json!({
                "product_id": product_id,
                "variant_id": updated.variant_id,
                "quantity": updated.quantity,
                "reserved": updated.reserved,
                "quantity_delta": quantity_delta,
                "reserved_delta": reserved_delta,
                "reason": input.reason,
            }),
        );
        if updated.quantity - updated.reserved <= updated.low_stock_threshold {
            state.emit(
                "product.inventory.low_stock",
                json!({
                    "product_id": product_id,
                    "variant_id": updated.variant_id,
                    "quantity": updated.quantity,
                    "reserved": updated.reserved,
                    "available": updated.quantity - updated.reserved,
                    "low_stock_threshold": updated.low_stock_threshold,
                }),
            );
        }
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Product file and version handlers (references only)
// ---------------------------------------------------------------------------

pub(crate) async fn list_product_files(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let _product = match require_product_in_scope(&state, path.as_str(), &_scope) {
        Ok(product) => product,
        Err(error) => {
            return json::respond(Err::<Value, UnifiedError>(error), false);
        }
    };
    let mut items: Vec<ProductFile> = state
        .files
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_product_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateProductFile>,
) -> HttpResponse {
    let result: UnifiedResult<ProductFile> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let product_id = path.into_inner();
        let _product = require_product_in_scope(&state, &product_id, &_scope)?;
        if input.software_id.is_none()
            && input.release_id.is_none()
            && input.file_id.is_none()
        {
            return Err(UnifiedError::bad_request(
                "At least one of software_id, release_id, or file_id is required",
            ));
        }
        if let Some(checksum) = &input.checksum_sha256 {
            validate_hex_checksum(&checksum.to_lowercase(), 64, "Checksum SHA-256")?;
        }
        let file = ProductFile {
            id: new_id(),
            product_id,
            software_id: input.software_id,
            software_version: input.software_version,
            release_id: input.release_id,
            file_id: input.file_id,
            filename: input.filename,
            checksum_sha256: input.checksum_sha256.map(|value| value.to_lowercase()),
            created_at: now(),
            updated_at: now(),
        };
        state.files.insert(file.id.clone(), file.clone());
        Ok(file)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn delete_product_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let record = state
            .files
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product file not found"))?;
        require_product_in_scope(&state, &record.product_id, &_scope)?;
        state
            .files
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Product file not found"))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_product_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let _product = match require_product_in_scope(&state, path.as_str(), &_scope) {
        Ok(product) => product,
        Err(error) => {
            return json::respond(Err::<Value, UnifiedError>(error), false);
        }
    };
    let mut items: Vec<ProductVersion> = state
        .versions
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    items.sort_by(|a, b| {
        let parsed_a = Version::parse(&a.version);
        let parsed_b = Version::parse(&b.version);
        match (parsed_a, parsed_b) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            _ => a.version.cmp(&b.version),
        }
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_product_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateProductVersion>,
) -> HttpResponse {
    let result: UnifiedResult<ProductVersion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let product_id = path.into_inner();
        let _product = require_product_in_scope(&state, &product_id, &_scope)?;
        Version::parse(&input.version).map_err(|_| {
            UnifiedError::bad_request(format!(
                "Version \"{}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH)",
                input.version
            ))
        })?;
        if state.versions.iter().any(|entry| {
            entry.value().product_id == product_id && entry.value().version == input.version
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has version \"{}\"",
                input.version
            )));
        }
        let version = ProductVersion {
            id: new_id(),
            product_id,
            version: input.version,
            software_id: input.software_id,
            release_id: input.release_id,
            changelog: input.changelog.unwrap_or_default(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .versions
            .insert(version.id.clone(), version.clone());
        Ok(version)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn delete_product_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let record = state
            .versions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product version not found"))?;
        require_product_in_scope(&state, &record.product_id, &_scope)?;
        state
            .versions
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Product version not found"))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Taxonomy handlers (same shapes as the software module)
// ---------------------------------------------------------------------------

pub(crate) async fn list_taxonomy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|node| {
            matches_search(&format!("{} {} {}", node.name, node.slug, node.description), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| node.kind == *kind)
                && params.filters.get("parent_id").is_none_or(|parent_id| {
                    node.parent_id.as_deref() == Some(parent_id.as_str())
                })
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
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let kind = params.filters.get("kind").map(String::as_str);
    if let Some(kind) = kind {
        if !TAXONOMY_KINDS.contains(&kind) {
            return json::respond(
                Err::<Value, UnifiedError>(UnifiedError::bad_request(
                    "Kind must be group or category",
                )),
                false,
            );
        }
    }
    let tree = build_tree(&state, None, kind);
    json::success_response(tree, 200)
}

pub(crate) async fn create_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
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
        if state
            .taxonomy
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Taxonomy node with slug \"{slug}\" already exists"
            )));
        }
        if let Some(parent_id) = &input.parent_id {
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == input.kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent taxonomy node not found for this kind",
                    ));
                }
            }
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
        Ok(node)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
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
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state.taxonomy.iter().any(|entry| {
                entry.value().slug == *slug && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Taxonomy node with slug \"{slug}\" already exists"
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
            // The path embeds the slug, so recompute this subtree.
            let node_id = node.id.clone();
            drop(node);
            recompute_subtree(&state, &node_id);
            return Ok(state
                .taxonomy
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .expect("taxonomy node still present"));
        }
        if let Some(description) = input.description {
            node.description = description;
        }
        if let Some(sort_order) = input.sort_order {
            node.sort_order = sort_order;
        }
        node.updated_at = now();
        Ok(node.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
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
        let referenced = state.products.iter().any(|entry| {
            (node.kind == "group" && entry.value().group_id.as_deref() == Some(path.as_str()))
                || (node.kind == "category"
                    && entry.value().category_id.as_deref() == Some(path.as_str()))
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Taxonomy node is referenced by products",
            ));
        }
        state.taxonomy.remove(path.as_str());
        state.emit(
            "product.taxonomy.deleted",
            serde_json::to_value(&node).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn move_taxonomy_node(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MoveTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let node = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if let Some(parent_id) = &input.parent_id {
            if parent_id == path.as_str() {
                return Err(UnifiedError::bad_request(
                    "A taxonomy node cannot be its own parent",
                ));
            }
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == node.kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent taxonomy node not found for this kind",
                    ));
                }
            }
            if is_descendant_of(&state, parent_id, path.as_str()) {
                return Err(UnifiedError::bad_request(
                    "A taxonomy node cannot move under its own descendant",
                ));
            }
        }
        let node_id = path.as_str().to_string();
        if let Some(mut entry) = state.taxonomy.get_mut(&node_id) {
            entry.parent_id = input.parent_id;
            if let Some(sort_order) = input.sort_order {
                entry.sort_order = sort_order;
            }
            entry.updated_at = now();
        }
        recompute_subtree(&state, &node_id);
        Ok(state
            .taxonomy
            .get(&node_id)
            .map(|entry| entry.value().clone())
            .expect("taxonomy node still present"))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_taxonomy_children(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    if !state.taxonomy.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Taxonomy node not found")),
            false,
        );
    }
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| entry.value().parent_id.as_deref() == Some(path.as_str()))
        .map(|entry| entry.value().clone())
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

// ---------------------------------------------------------------------------
// Tag handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_tags(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<Tag> = state
        .tags
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tag| matches_search(&format!("{} {}", tag.name, tag.slug), &params))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    body: web::Json<CreateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tag name must not be empty"));
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
        Ok(tag)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
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
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
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
                return Err(UnifiedError::bad_request("Tag name must not be empty"));
            }
            tag.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            tag.slug = slug;
        }
        tag.updated_at = now();
        Ok(tag.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let tag = state
            .tags
            .remove(path.as_str())
            .map(|(_, tag)| tag)
            .ok_or_else(|| UnifiedError::not_found("Tag not found"))?;
        for mut entry in state.product_tags.iter_mut() {
            entry.value_mut().remove(path.as_str());
        }
        state.emit(
            "product.taxonomy.deleted",
            serde_json::to_value(&tag).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn tag_cloud(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "product_count", "created_at"], "product_count");
    let mut counts: HashMap<String, u64> = HashMap::new();
    for entry in state.product_tags.iter() {
        // Only tags on products in the caller scope count: a
        // company caller must not see tag usage on another
        // company's products, and an unresolved caller sees
        // global products only.
        let product_in_scope = state
            .products
            .get(entry.key().as_str())
            .map(|product| in_company_scope(&product.value().vendor_company_id, &_scope))
            .unwrap_or(false);
        if !product_in_scope {
            continue;
        }
        for tag_id in entry.value().iter() {
            *counts.entry(tag_id.clone()).or_insert(0) += 1;
        }
    }
    let mut items: Vec<TagCount> = state
        .tags
        .iter()
        .map(|entry| entry.value().clone())
        .map(|tag| TagCount {
            product_count: counts.get(&tag.id).copied().unwrap_or(0),
            tag,
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.tag.name.cmp(&b.tag.name)),
        "created_at" => items.sort_by(|a, b| a.tag.created_at.cmp(&b.tag.created_at)),
        _ => items.sort_by(|a, b| a.product_count.cmp(&b.product_count)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn list_product_tags(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<Tag>> = (|| {
        let _product = require_product_in_scope(&state, path.as_str(), &_scope)?;
        let tag_ids = state
            .product_tags
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut tags: Vec<Tag> = tag_ids
            .iter()
            .filter_map(|id| state.tags.get(id).map(|entry| entry.value().clone()))
            .collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(tags, &params))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn attach_product_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachTag>,
) -> HttpResponse {
    let result: UnifiedResult<Vec<Tag>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let _product = require_product_in_scope(&state, path.as_str(), &_scope)?;
        if !state.tags.contains_key(&input.tag_id) {
            return Err(UnifiedError::not_found("Tag not found"));
        }
        state
            .product_tags
            .entry(path.as_str().to_string())
            .or_default()
            .insert(input.tag_id);
        let tag_ids = state
            .product_tags
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut tags: Vec<Tag> = tag_ids
            .iter()
            .filter_map(|id| state.tags.get(id).map(|entry| entry.value().clone()))
            .collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tags)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn detach_product_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (product_id, tag_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let _product = require_product_in_scope(&state, &product_id, &_scope)?;
        let removed = state
            .product_tags
            .get_mut(&product_id)
            .is_some_and(|mut tags| tags.remove(&tag_id));
        if removed {
            Ok(json!({ "deleted": true, "product_id": product_id, "tag_id": tag_id }))
        } else {
            Err(UnifiedError::not_found("Tag is not attached to this product"))
        }
    })();
    json::respond(result, ctx.is_debug())
}
