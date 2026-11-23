//! HTTP handlers for options, variants, bundles and pricing rules.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::modules::{ProductCategory};
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::{HashMap};
use std::sync::{Arc};
use super::money::resolve_money;
use super::types::{AddBundleItem, BundleItem, CreatePriceTier, CreateProductOption, CreateProductVariant, CreateRegionalPrice, CreateTaxRule, CreateUsagePrice, PERM_PRODUCTS_CREATE, PERM_PRODUCTS_MANAGE, PERM_PRODUCTS_READ, PriceTier, ProductDetail, ProductOption, ProductVariant, RegionalPrice, TaxRule, UpdateBundleRule, UpdatePriceTier, UpdateProductVariant, UpdateRegionalPrice, UpdateTaxRule, UpdateUsagePrice, UsagePrice, default_tier_status, default_variant_status};
use super::state::ProductsState;
use super::validation::{matches_search, new_id, now, paginate, respond_created, validate_billing_period, validate_bundle_discount_bps, validate_bundle_rule, validate_region, validate_tax_rate_bps};
use super::helpers::{
    caller, caller_company, require_permission, require_product_in_scope,
    require_record_in_company_scope, require_tenant,
};

/// List the sellable modules as product offerings
/// (`GET /api/v1/products/module-offerings`). The offerings are
/// derived from the module registry in memory — no database row
/// backs them — so this endpoint reads nothing from the product
/// catalogue. Requires the `products:read` permission (fail-closed
/// through the `role.check` hook). Supports the standard pagination
/// parameters plus an exact `category` filter, which is validated
/// against the closed `ProductCategory` set and rejected with 400
/// when unknown.
pub(crate) async fn list_module_offerings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ)?;
        let params = query.into_inner();
        let mut offerings = ctx.modules.product_offerings();
        if let Some(category) = params.filters.get("category") {
            let category = ProductCategory::from_str(category).ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Unknown product category \"{category}\" (expected one of: {})",
                    ProductCategory::ALL.join(", ")
                ))
            })?;
            offerings.retain(|offering| offering.category == category.as_str());
        }
        offerings.retain(|offering| {
            matches_search(
                &format!("{} {} {}", offering.id, offering.name, offering.description),
                &params,
            )
        });
        let sort = params.allowed_sort(&["name", "id", "category", "version"], "name");
        match sort.as_str() {
            "id" => offerings.sort_by(|a, b| a.id.cmp(b.id)),
            "category" => {
                offerings.sort_by(|a, b| a.category.cmp(b.category).then(a.name.cmp(b.name)))
            }
            "version" => offerings.sort_by(|a, b| a.version.cmp(b.version)),
            _ => offerings.sort_by(|a, b| a.name.cmp(b.name)),
        }
        if params.normalized().order == "desc" {
            offerings.reverse();
        }
        Ok(paginate(offerings, &params).to_json())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Option handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_options(
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
    let mut items: Vec<ProductOption> = state
        .options
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|option| matches_search(&option.name, &params))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_option(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateProductOption>,
) -> HttpResponse {
    let result: UnifiedResult<ProductOption> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Option name must not be empty"));
        }
        if input.values.is_empty()
            || input.values.iter().any(|value| value.trim().is_empty())
        {
            return Err(UnifiedError::bad_request(
                "Option values must not be empty",
            ));
        }
        let _product = require_product_in_scope(&state, path.as_str(), &_scope)?;
        if state.options.iter().any(|entry| {
            entry.value().product_id == path.as_str()
                && entry.value().name.eq_ignore_ascii_case(input.name.trim())
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has an option named \"{}\"",
                input.name.trim()
            )));
        }
        let option = ProductOption {
            id: new_id(),
            product_id: path.into_inner(),
            name: input.name.trim().to_string(),
            values: input.values.iter().map(|value| value.trim().to_string()).collect(),
            required: input.required.unwrap_or(false),
            created_at: now(),
            updated_at: now(),
        };
        state.options.insert(option.id.clone(), option.clone());
        Ok(option)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn delete_option(
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
        let option = state
            .options
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Option not found"))?;
        require_product_in_scope(&state, &option.product_id, &_scope)?;
        let in_use = state.variants.iter().any(|entry| {
            entry.value().product_id == option.product_id
                && entry.value().option_values.contains_key(&option.name)
        });
        if in_use {
            return Err(UnifiedError::conflict(
                "Option is used by variants; delete the variants first",
            ));
        }
        state.options.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Variant handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_variants(
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
    let mut items: Vec<ProductVariant> = state
        .variants
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|variant| {
            matches_search(&format!("{} {}", variant.sku, variant.label), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| variant.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.sku.cmp(&b.sku));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) fn validate_variant_options(
    state: &ProductsState,
    product_id: &str,
    option_values: &HashMap<String, String>,
) -> UnifiedResult<()> {
    // Every named option must exist on the product and the chosen
    // value must be one of its allowed values. Required options
    // must all be present.
    let options: Vec<ProductOption> = state
        .options
        .iter()
        .filter(|entry| entry.value().product_id == product_id)
        .map(|entry| entry.value().clone())
        .collect();
    for (name, value) in option_values {
        match options.iter().find(|option| option.name == *name) {
            Some(option) => {
                if !option.values.iter().any(|allowed| allowed == value) {
                    return Err(UnifiedError::bad_request(format!(
                        "Value \"{value}\" is not allowed for option \"{name}\""
                    )));
                }
            }
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Product has no option named \"{name}\""
                )));
            }
        }
    }
    for option in &options {
        if option.required && !option_values.contains_key(&option.name) {
            return Err(UnifiedError::bad_request(format!(
                "Option \"{}\" is required",
                option.name
            )));
        }
    }
    Ok(())
}

pub(crate) async fn create_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateProductVariant>,
) -> HttpResponse {
    let result: UnifiedResult<ProductVariant> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let product_id = path.into_inner();
        if input.sku.trim().is_empty() {
            return Err(UnifiedError::bad_request("SKU must not be empty"));
        }
        let _product = require_product_in_scope(&state, &product_id, &_scope)?;
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Variant status must be active or disabled",
                ));
            }
        }
        validate_variant_options(&state, &product_id, &input.option_values)?;
        if state
            .variants
            .iter()
            .any(|entry| entry.value().sku == input.sku.trim())
        {
            return Err(UnifiedError::conflict(format!(
                "Variant with SKU \"{}\" already exists",
                input.sku.trim()
            )));
        }
        let variant = ProductVariant {
            id: new_id(),
            product_id,
            sku: input.sku.trim().to_string(),
            label: input
                .label
                .unwrap_or_else(|| input.sku.trim().to_string()),
            option_values: input.option_values,
            price_delta_minor: input.price_delta_minor.unwrap_or(0),
            status: input.status.unwrap_or_else(default_variant_status),
            created_at: now(),
            updated_at: now(),
        };
        state.variants.insert(variant.id.clone(), variant.clone());
        Ok(variant)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_variant(
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
    let result: UnifiedResult<ProductVariant> = state
        .variants
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Variant not found"))
        .and_then(|variant| {
            require_product_in_scope(&state, &variant.product_id, &_scope)?;
            Ok(variant)
        });
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProductVariant>,
) -> HttpResponse {
    let result: UnifiedResult<ProductVariant> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let new_sku = input.sku.as_ref().map(|sku| sku.trim().to_string());
        if let Some(sku) = &new_sku {
            if sku.is_empty() {
                return Err(UnifiedError::bad_request("SKU must not be empty"));
            }
            if state
                .variants
                .iter()
                .any(|entry| entry.value().sku == *sku && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Variant with SKU \"{sku}\" already exists"
                )));
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Variant status must be active or disabled",
                ));
            }
        }
        let mut variant = state
            .variants
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))?;
        require_product_in_scope(&state, &variant.product_id, &_scope)?;
        if let Some(option_values) = &input.option_values {
            validate_variant_options(&state, &variant.product_id, option_values)?;
            variant.option_values = option_values.clone();
        }
        if let Some(sku) = new_sku {
            variant.sku = sku;
        }
        if let Some(label) = input.label {
            variant.label = label;
        }
        if let Some(delta) = input.price_delta_minor {
            variant.price_delta_minor = delta;
        }
        if let Some(status) = input.status {
            variant.status = status;
        }
        variant.updated_at = now();
        Ok(variant.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_variant(
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
        let variant = state
            .variants
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))?;
        require_product_in_scope(&state, &variant.product_id, &_scope)?;
        if state
            .inventory
            .iter()
            .any(|entry| entry.value().variant_id.as_deref() == Some(path.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Variant has inventory records; delete them first",
            ));
        }
        state
            .variants
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Bundle handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_bundle_items(
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
    let items: Vec<BundleItem> = state
        .bundle_items
        .iter()
        .filter(|entry| entry.value().bundle_product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn add_bundle_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddBundleItem>,
) -> HttpResponse {
    let result: UnifiedResult<BundleItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let bundle_id = path.into_inner();
        let bundle = state
            .products
            .get(&bundle_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&bundle.vendor_company_id, &_scope)?;
        if bundle.kind != "bundle" {
            return Err(UnifiedError::bad_request(
                "Only bundle products can contain bundle items",
            ));
        }
        let quantity = input.quantity.unwrap_or(1);
        if quantity < 1 {
            return Err(UnifiedError::bad_request("Quantity must be at least 1"));
        }
        let child = state
            .products
            .get(&input.child_product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Child product not found"))?;
        require_record_in_company_scope(&child.vendor_company_id, &_scope)?;
        if let Some(override_minor) = input.price_override_minor {
            if override_minor < 0 {
                return Err(UnifiedError::bad_request(
                    "Price override must not be negative",
                ));
            }
            if child.price.currency != bundle.price.currency {
                return Err(UnifiedError::bad_request(
                    "Bundle child currency does not match the bundle currency",
                ));
            }
        }
        if state.bundle_would_cycle(&bundle_id, &input.child_product_id) {
            return Err(UnifiedError::bad_request(
                "Adding this product would create a bundle cycle",
            ));
        }
        if state.bundle_items.iter().any(|entry| {
            entry.value().bundle_product_id == bundle_id
                && entry.value().child_product_id == input.child_product_id
        }) {
            return Err(UnifiedError::conflict(
                "Product is already part of this bundle",
            ));
        }
        let item = BundleItem {
            id: new_id(),
            bundle_product_id: bundle_id.clone(),
            child_product_id: input.child_product_id,
            quantity,
            price_override_minor: input.price_override_minor,
            created_at: now(),
            updated_at: now(),
        };
        state.bundle_items.insert(item.id.clone(), item.clone());
        state.emit(
            "product.bundle.updated",
            json!({ "product_id": bundle_id, "bundle_item_id": item.id }),
        );
        Ok(item)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn delete_bundle_item(
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
        let item = state
            .bundle_items
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Bundle item not found"))?;
        require_product_in_scope(&state, &item.bundle_product_id, &_scope)?;
        state.bundle_items.remove(path.as_str());
        state.emit(
            "product.bundle.updated",
            json!({ "product_id": item.bundle_product_id, "deleted_bundle_item_id": item.id }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_bundle_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBundleRule>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        validate_bundle_rule(&input.bundle_rule)?;
        if input.bundle_rule == "discount_percent" {
            let discount = input.bundle_discount_bps.unwrap_or(0);
            validate_bundle_discount_bps(discount)?;
        } else if input.bundle_discount_bps.is_some() {
            return Err(UnifiedError::bad_request(
                "A discount only applies to the discount_percent bundle rule",
            ));
        }
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        if product.kind != "bundle" {
            return Err(UnifiedError::bad_request(
                "Only bundle products have a bundle pricing rule",
            ));
        }
        product.bundle_rule = input.bundle_rule;
        product.bundle_discount_bps = if product.bundle_rule == "discount_percent" {
            Some(input.bundle_discount_bps.unwrap_or(0))
        } else {
            None
        };
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.bundle.updated", detail_value);
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Pricing tier handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_tiers(
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
    let mut items: Vec<PriceTier> = state
        .tiers
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|tier| {
            matches_search(&format!("{} {}", tier.slug, tier.label), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| tier.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.slug.cmp(&b.slug));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePriceTier>,
) -> HttpResponse {
    let result: UnifiedResult<PriceTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let product_id = path.into_inner();
        let product = require_product_in_scope(&state, &product_id, &_scope)?;
        if input.slug.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tier slug must not be empty"));
        }
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Tier status must be active or disabled",
                ));
            }
        }
        let price = resolve_money(&input.price, Some(&product.price.currency))?;
        if price.currency != product.price.currency {
            return Err(UnifiedError::bad_request(
                "Tier currency must match the product currency",
            ));
        }
        let renewal_price = match &input.renewal_price {
            Some(renewal) => {
                let renewal = resolve_money(renewal, Some(&product.price.currency))?;
                if renewal.currency != product.price.currency {
                    return Err(UnifiedError::bad_request(
                        "Renewal price currency must match the product currency",
                    ));
                }
                Some(renewal)
            }
            None => None,
        };
        if state.tiers.iter().any(|entry| {
            entry.value().product_id == product_id && entry.value().slug == input.slug.trim()
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has a tier with slug \"{}\"",
                input.slug.trim()
            )));
        }
        let seats = input.seats.unwrap_or(1);
        if seats < 1 {
            return Err(UnifiedError::bad_request("Tier seats must be at least 1"));
        }
        let tier = PriceTier {
            id: new_id(),
            product_id,
            slug: input.slug.trim().to_string(),
            label: input.label.unwrap_or_else(|| input.slug.trim().to_string()),
            price,
            renewal_price,
            billing_period: input.billing_period,
            trial_days: input.trial_days.unwrap_or(0),
            seats,
            per_seat: input.per_seat.unwrap_or(false),
            download_limit: input.download_limit.unwrap_or(10),
            features: input.features,
            status: input.status.unwrap_or_else(default_tier_status),
            created_at: now(),
            updated_at: now(),
        };
        state.tiers.insert(tier.id.clone(), tier.clone());
        Ok(tier)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_tier(
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
    let result: UnifiedResult<PriceTier> = state
        .tiers
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))
        .and_then(|tier| {
            require_product_in_scope(&state, &tier.product_id, &_scope)?;
            Ok(tier)
        });
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePriceTier>,
) -> HttpResponse {
    let result: UnifiedResult<PriceTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let new_slug = input.slug.as_ref().map(|slug| slug.trim().to_string());
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Tier slug must not be empty"));
            }
        }
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Tier status must be active or disabled",
                ));
            }
        }
        let new_price = match &input.price {
            Some(price) => Some(resolve_money(price, None)?),
            None => None,
        };
        let new_renewal = match &input.renewal_price {
            Some(renewal) => Some(resolve_money(renewal, None)?),
            None => None,
        };
        let mut tier = state
            .tiers
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        require_product_in_scope(&state, &tier.product_id, &_scope)?;
        if let Some(slug) = new_slug {
            if state.tiers.iter().any(|entry| {
                entry.value().product_id == tier.product_id
                    && entry.value().slug == slug
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Product already has a tier with slug \"{slug}\""
                )));
            }
            tier.slug = slug;
        }
        if let Some(label) = input.label {
            tier.label = label;
        }
        if let Some(price) = new_price {
            if price.currency != tier.price.currency {
                return Err(UnifiedError::bad_request(
                    "Tier currency must match the product currency",
                ));
            }
            tier.price = price;
        }
        if let Some(renewal) = new_renewal {
            if renewal.currency != tier.price.currency {
                return Err(UnifiedError::bad_request(
                    "Renewal price currency must match the product currency",
                ));
            }
            tier.renewal_price = Some(renewal);
        }
        if let Some(period) = input.billing_period {
            tier.billing_period = Some(period);
        }
        if let Some(trial_days) = input.trial_days {
            tier.trial_days = trial_days;
        }
        if let Some(seats) = input.seats {
            if seats < 1 {
                return Err(UnifiedError::bad_request("Tier seats must be at least 1"));
            }
            tier.seats = seats;
        }
        if let Some(per_seat) = input.per_seat {
            tier.per_seat = per_seat;
        }
        if let Some(download_limit) = input.download_limit {
            tier.download_limit = download_limit;
        }
        if let Some(features) = input.features {
            tier.features = features;
        }
        if let Some(status) = input.status {
            tier.status = status;
        }
        tier.updated_at = now();
        Ok(tier.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_tier(
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
        let tier = state
            .tiers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        require_product_in_scope(&state, &tier.product_id, &_scope)?;
        state
            .tiers
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Regional price handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_regional_prices(
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
    let mut items: Vec<RegionalPrice> = state
        .regional_prices
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    items.sort_by(|a, b| a.region.cmp(&b.region));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_regional_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateRegionalPrice>,
) -> HttpResponse {
    let result: UnifiedResult<RegionalPrice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let product_id = path.into_inner();
        let _product = require_product_in_scope(&state, &product_id, &_scope)?;
        let region = input.region.trim().to_uppercase();
        validate_region(&region)?;
        let price = resolve_money(&input.price, None)?;
        if state.regional_prices.iter().any(|entry| {
            entry.value().product_id == product_id && entry.value().region == region
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has a regional price for \"{region}\""
            )));
        }
        let regional = RegionalPrice {
            id: new_id(),
            product_id,
            region,
            price,
            created_at: now(),
            updated_at: now(),
        };
        state
            .regional_prices
            .insert(regional.id.clone(), regional.clone());
        Ok(regional)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn update_regional_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRegionalPrice>,
) -> HttpResponse {
    let result: UnifiedResult<RegionalPrice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let new_region = input.region.as_ref().map(|region| region.trim().to_uppercase());
        if let Some(region) = &new_region {
            validate_region(region)?;
        }
        let new_price = match &input.price {
            Some(price) => Some(resolve_money(price, None)?),
            None => None,
        };
        let mut regional = state
            .regional_prices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Regional price not found"))?;
        require_product_in_scope(&state, &regional.product_id, &_scope)?;
        if let Some(region) = new_region {
            if state.regional_prices.iter().any(|entry| {
                entry.value().product_id == regional.product_id
                    && entry.value().region == region
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Product already has a regional price for \"{region}\""
                )));
            }
            regional.region = region;
        }
        if let Some(price) = new_price {
            regional.price = price;
        }
        regional.updated_at = now();
        Ok(regional.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_regional_price(
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
        let regional = state
            .regional_prices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Regional price not found"))?;
        require_product_in_scope(&state, &regional.product_id, &_scope)?;
        state
            .regional_prices
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Regional price not found"))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Usage price handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_usage_prices(
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
    let mut items: Vec<UsagePrice> = state
        .usage_prices
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|usage| matches_search(&usage.metric, &params))
        .collect();
    items.sort_by(|a, b| a.metric.cmp(&b.metric));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_usage_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateUsagePrice>,
) -> HttpResponse {
    let result: UnifiedResult<UsagePrice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let product_id = path.into_inner();
        let product = require_product_in_scope(&state, &product_id, &_scope)?;
        if input.metric.trim().is_empty() {
            return Err(UnifiedError::bad_request("Metric must not be empty"));
        }
        let unit = resolve_money(&input.unit_price, Some(&product.price.currency))?;
        if state.usage_prices.iter().any(|entry| {
            entry.value().product_id == product_id
                && entry.value().metric == input.metric.trim()
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has a usage price for metric \"{}\"",
                input.metric.trim()
            )));
        }
        let usage = UsagePrice {
            id: new_id(),
            product_id,
            metric: input.metric.trim().to_string(),
            unit_price_minor: unit.amount_minor,
            currency: unit.currency,
            included_units: input.included_units.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.usage_prices.insert(usage.id.clone(), usage.clone());
        Ok(usage)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn update_usage_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateUsagePrice>,
) -> HttpResponse {
    let result: UnifiedResult<UsagePrice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let new_unit = match &input.unit_price {
            Some(unit) => Some(resolve_money(unit, None)?),
            None => None,
        };
        let mut usage = state
            .usage_prices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Usage price not found"))?;
        require_product_in_scope(&state, &usage.product_id, &_scope)?;
        if let Some(metric) = input.metric {
            if metric.trim().is_empty() {
                return Err(UnifiedError::bad_request("Metric must not be empty"));
            }
            if state.usage_prices.iter().any(|entry| {
                entry.value().product_id == usage.product_id
                    && entry.value().metric == metric.trim()
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Product already has a usage price for metric \"{}\"",
                    metric.trim()
                )));
            }
            usage.metric = metric.trim().to_string();
        }
        if let Some(unit) = new_unit {
            if unit.currency != usage.currency {
                return Err(UnifiedError::bad_request(
                    "Usage price currency must not change",
                ));
            }
            usage.unit_price_minor = unit.amount_minor;
        }
        if let Some(included) = input.included_units {
            usage.included_units = included;
        }
        usage.updated_at = now();
        Ok(usage.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_usage_price(
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
        let usage = state
            .usage_prices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Usage price not found"))?;
        require_product_in_scope(&state, &usage.product_id, &_scope)?;
        state
            .usage_prices
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Usage price not found"))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Tax rule handlers (global)
// ---------------------------------------------------------------------------

pub(crate) async fn list_tax_rules(
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
    let mut items: Vec<TaxRule> = state
        .tax_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            matches_search(&format!("{} {}", rule.country, rule.name), &params)
                && params
                    .filters
                    .get("country")
                    .is_none_or(|country| rule.country.eq_ignore_ascii_case(country))
        })
        .collect();
    items.sort_by(|a, b| a.country.cmp(&b.country));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_tax_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        let country = input.country.trim().to_uppercase();
        validate_region(&country)?;
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tax rule name must not be empty"));
        }
        validate_tax_rate_bps(input.rate_bps)?;
        if state
            .tax_rules
            .iter()
            .any(|entry| entry.value().country == country)
        {
            return Err(UnifiedError::conflict(format!(
                "A tax rule already exists for country \"{country}\""
            )));
        }
        let rule = TaxRule {
            id: new_id(),
            country,
            name: input.name.trim().to_string(),
            rate_bps: input.rate_bps,
            created_at: now(),
            updated_at: now(),
        };
        state.tax_rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn update_tax_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        let new_country = input.country.as_ref().map(|country| country.trim().to_uppercase());
        if let Some(country) = &new_country {
            validate_region(country)?;
        }
        if let Some(rate_bps) = input.rate_bps {
            validate_tax_rate_bps(rate_bps)?;
        }
        let mut rule = state
            .tax_rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if let Some(country) = new_country {
            if state.tax_rules.iter().any(|entry| {
                entry.value().country == country && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A tax rule already exists for country \"{country}\""
                )));
            }
            rule.country = country;
        }
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Tax rule name must not be empty"));
            }
            rule.name = name.trim().to_string();
        }
        if let Some(rate_bps) = input.rate_bps {
            rule.rate_bps = rate_bps;
        }
        rule.updated_at = now();
        Ok(rule.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_tax_rule(
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
        state
            .tax_rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))
    })();
    json::respond(result, ctx.is_debug())
}
