//! HTTP handlers for products: CRUD, lifecycle, pricing, support.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::{Arc};
use super::helpers::{
    caller, caller_company, check_capability, in_company_scope, require_company_match,
    require_company_match_for, require_permission, require_product_in_scope,
    require_record_in_company_scope, require_tenant,
};
use super::money::resolve_money;
use super::types::{CreateProduct, CreateSupportTicket, EntitlementCheckRequest, PERM_PRODUCTS_CREATE, PERM_PRODUCTS_MANAGE, PERM_PRODUCTS_READ, PRODUCT_STATUSES, PriceQuote, PriceQuoteRequest, Product, ProductDetail, SubscriptionPlanView, TransitionProduct, UpdateProduct, default_bundle_rule};
use super::state::ProductsState;
use super::validation::{matches_search, new_id, now, paginate, resolve_subscription_settings, respond_created, slugify, validate_billing_period, validate_bundle_discount_bps, validate_bundle_rule, validate_kind, validate_license_template, validate_pricing_model, validate_product_refs, validate_region, validate_status_transition};

// ---------------------------------------------------------------------------
// Product handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_products(
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
    let sort = params.allowed_sort(&["name", "slug", "status", "created_at"], "created_at");
    let tag_filter = params.filters.get("tag").cloned();
    let mut items: Vec<Product> = state
        .products
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|product| {
                in_company_scope(&product.vendor_company_id, &_scope)
                && matches_search(
                &format!("{} {} {}", product.name, product.slug, product.description),
                &params,
            ) && params.filters.get("kind").is_none_or(|kind| {
                product.kind == *kind
            }) && params.filters.get("status").is_none_or(|status| {
                product.status == *status
            }) && params.filters.get("pricing_model").is_none_or(|model| {
                product.pricing_model == *model
            }) && params.filters.get("vendor_company_id").is_none_or(|vendor| {
                product.vendor_company_id.as_deref() == Some(vendor.as_str())
            }) && params.filters.get("group_id").is_none_or(|group_id| {
                product.group_id.as_deref() == Some(group_id.as_str())
            }) && params.filters.get("category_id").is_none_or(|category_id| {
                product.category_id.as_deref() == Some(category_id.as_str())
            }) && params.filters.get("currency").is_none_or(|currency| {
                product.price.currency.eq_ignore_ascii_case(currency)
            }) && params.filters.get("listed").is_none_or(|flag| {
                product.listed == (*flag == "true" || *flag == "1")
            }) && tag_filter.as_ref().is_none_or(|tag_id| {
                state
                    .product_tags
                    .get(&product.id)
                    .is_some_and(|tags| tags.contains(tag_id.as_str()))
            })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_product(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    body: web::Json<CreateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        validate_kind(&input.kind)?;
        validate_pricing_model(&input.pricing_model)?;
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if input.pricing_model == "subscription" && input.billing_period.is_none() {
            return Err(UnifiedError::bad_request(
                "Subscription products require a billing period",
            ));
        }
        if input.kind == "bundle" {
            if let Some(rule) = &input.bundle_rule {
                validate_bundle_rule(rule)?;
            }
            if let Some(discount) = input.bundle_discount_bps {
                validate_bundle_discount_bps(discount)?;
            }
        } else if input.bundle_rule.is_some() || input.bundle_discount_bps.is_some() {
            return Err(UnifiedError::bad_request(
                "Bundle pricing rules only apply to bundle products",
            ));
        }
        let price = resolve_money(&input.price, None)?;
        let renewal_price = match &input.renewal_price {
            Some(renewal) => Some(resolve_money(renewal, Some(&price.currency))?),
            None => None,
        };
        if let Some(renewal) = &renewal_price {
            if renewal.currency != price.currency {
                return Err(UnifiedError::bad_request(
                    "Renewal price currency must match the base price currency",
                ));
            }
        }
        if let Some(template) = &input.license_template {
            validate_license_template(template)?;
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the product name",
            ));
        }
        if state
            .products
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Product with slug \"{slug}\" already exists"
            )));
        }
        validate_product_refs(
            &state,
            input.vendor_company_id.as_deref(),
            input.group_id.as_deref(),
            input.category_id.as_deref(),
        )?;
        for tag_id in &input.tag_ids {
            if !state.tags.contains_key(tag_id.as_str()) {
                return Err(UnifiedError::not_found(format!(
                    "Tag \"{tag_id}\" not found"
                )));
            }
        }
        if input.trial_days.unwrap_or(0) > 3650 {
            return Err(UnifiedError::bad_request(
                "Trial days must be at most 3650",
            ));
        }
        let (proration_policy, grace_period_days, dunning_max_retries, dunning_retry_interval_days) =
            resolve_subscription_settings(
                &input.pricing_model,
                input.proration_policy,
                input.grace_period_days,
                input.dunning_max_retries,
                input.dunning_retry_interval_days,
            )?;
        let vendor_company_id = require_company_match(&_tenant, &req, &input.vendor_company_id)?;
        let product = Product {
            id: new_id(),
            slug,
            name: input.name.trim().to_string(),
            kind: input.kind,
            description: input.description.unwrap_or_default(),
            status: "draft".to_string(),
            pricing_model: input.pricing_model,
            billing_period: input.billing_period,
            price,
            renewal_price,
            trial_days: input.trial_days.unwrap_or(0),
            proration_policy,
            grace_period_days,
            dunning_max_retries,
            dunning_retry_interval_days,
            vendor_company_id,
            group_id: input.group_id,
            category_id: input.category_id,
            software_id: input.software_id,
            license_template: input.license_template.unwrap_or_default(),
            support_department_id: input.support_department_id,
            support_department_name: input.support_department_name,
            bundle_rule: input.bundle_rule.unwrap_or_else(default_bundle_rule),
            bundle_discount_bps: input.bundle_discount_bps,
            listed: false,
            created_at: now(),
            updated_at: now(),
        };
        state.products.insert(product.id.clone(), product.clone());
        if !input.tag_ids.is_empty() {
            state.product_tags.insert(
                product.id.clone(),
                input.tag_ids.iter().cloned().collect(),
            );
        }
        let detail = state.product_detail(&product);
        state.emit(
            "product.created",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_product(
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
    let result: UnifiedResult<ProductDetail> = state
        .products
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Product not found"))
        .and_then(|product| {
            require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
            Ok(state.product_detail(&product))
        });
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_product(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock:
        // iterating the map while holding a RefMut on one of its
        // entries would deadlock on the shard lock.
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state
                .products
                .iter()
                .any(|entry| entry.value().slug == *slug && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Product with slug \"{slug}\" already exists"
                )));
            }
        }
        // Resolve money inputs before touching the stored product so
        // a rejected update never leaves a half-mutated record.
        let new_price = match &input.price {
            Some(price) => Some(resolve_money(price, None)?),
            None => None,
        };
        let new_renewal_price = match &input.renewal_price {
            Some(renewal) => Some(resolve_money(renewal, None)?),
            None => None,
        };
        if let Some(kind) = &input.kind {
            validate_kind(kind)?;
        }
        if let Some(model) = &input.pricing_model {
            validate_pricing_model(model)?;
        }
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if let Some(template) = &input.license_template {
            validate_license_template(template)?;
        }
        // Every remaining fallible check runs here, against a
        // snapshot of the stored product and before the write lock is
        // taken, so a rejected update never leaves a half-mutated
        // record behind.
        let existing = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&existing.vendor_company_id, &_scope)?;
        // A newly-named vendor must match the caller scope; omitting
        // it keeps the stored vendor (the returned value is
        // discarded — only the check matters here).
        require_company_match_for(&_scope, &input.vendor_company_id)?;
        if let Some(kind) = &input.kind {
            if *kind != "bundle" && existing.kind == "bundle" {
                // A bundle keeps its bundle items; changing a bundle
                // into another kind is refused while it still has
                // items, so pricing can never dangle.
                let has_items = state
                    .bundle_items
                    .iter()
                    .any(|entry| entry.value().bundle_product_id == existing.id);
                if has_items {
                    return Err(UnifiedError::conflict(
                        "Product has bundle items; remove them before changing its kind",
                    ));
                }
            }
        }
        let effective_model = input
            .pricing_model
            .as_deref()
            .unwrap_or(existing.pricing_model.as_str());
        let effective_period = input
            .billing_period
            .as_deref()
            .or(existing.billing_period.as_deref());
        if effective_model == "subscription" && effective_period.is_none() {
            return Err(UnifiedError::bad_request(
                "Subscription products require a billing period",
            ));
        }
        let effective_currency = new_price
            .as_ref()
            .map(|price| price.currency.clone())
            .unwrap_or_else(|| existing.price.currency.clone());
        if let Some(renewal) = &new_renewal_price {
            if renewal.currency != effective_currency {
                return Err(UnifiedError::bad_request(
                    "Renewal price currency must match the base price currency",
                ));
            }
        }
        if let Some(trial_days) = input.trial_days {
            if trial_days > 3650 {
                return Err(UnifiedError::bad_request(
                    "Trial days must be at most 3650",
                ));
            }
        }
        // Subscription plan fields resolve against the effective
        // pricing model. When the model stays (or becomes)
        // `subscription`, unset fields keep the stored values; when
        // it is anything else, only caller-supplied values are
        // validated (non-default values are rejected) and the stored
        // plan resets to its defaults.
        let (new_proration_policy, new_grace_period_days, new_dunning_max_retries, new_dunning_retry_interval_days) =
            if effective_model == "subscription" {
                resolve_subscription_settings(
                    effective_model,
                    input
                        .proration_policy
                        .clone()
                        .or(Some(existing.proration_policy.clone())),
                    input.grace_period_days.or(Some(existing.grace_period_days)),
                    input
                        .dunning_max_retries
                        .or(Some(existing.dunning_max_retries)),
                    input
                        .dunning_retry_interval_days
                        .or(Some(existing.dunning_retry_interval_days)),
                )?
            } else {
                resolve_subscription_settings(
                    effective_model,
                    input.proration_policy.clone(),
                    input.grace_period_days,
                    input.dunning_max_retries,
                    input.dunning_retry_interval_days,
                )?
            };
        let effective_kind = input.kind.as_deref().unwrap_or(existing.kind.as_str());
        if effective_kind != "bundle"
            && (input.bundle_rule.is_some() || input.bundle_discount_bps.is_some())
        {
            return Err(UnifiedError::bad_request(
                "Bundle pricing rules only apply to bundle products",
            ));
        }
        if effective_kind == "bundle" {
            if let Some(rule) = &input.bundle_rule {
                validate_bundle_rule(rule)?;
            }
            if let Some(discount) = input.bundle_discount_bps {
                validate_bundle_discount_bps(discount)?;
            }
        }
        if let Some(status) = &input.status {
            if !PRODUCT_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Status must be one of: {}",
                    PRODUCT_STATUSES.join(", ")
                )));
            }
            validate_status_transition(&existing.status, status)?;
        }
        validate_product_refs(
            &state,
            input.vendor_company_id.as_deref(),
            input.group_id.as_deref(),
            input.category_id.as_deref(),
        )?;
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            product.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            product.slug = slug;
        }
        if let Some(kind) = input.kind {
            product.kind = kind;
        }
        if let Some(description) = input.description {
            product.description = description;
        }
        if let Some(model) = input.pricing_model {
            product.pricing_model = model;
        }
        if let Some(period) = input.billing_period {
            product.billing_period = Some(period);
        }
        let old_price_minor = product.price.amount_minor;
        let old_currency = product.price.currency.clone();
        if let Some(price) = new_price {
            product.price = price;
        }
        if let Some(renewal) = new_renewal_price {
            product.renewal_price = Some(renewal);
        }
        if let Some(trial_days) = input.trial_days {
            product.trial_days = trial_days;
        }
        product.proration_policy = new_proration_policy;
        product.grace_period_days = new_grace_period_days;
        product.dunning_max_retries = new_dunning_max_retries;
        product.dunning_retry_interval_days = new_dunning_retry_interval_days;
        if let Some(vendor) = input.vendor_company_id {
            product.vendor_company_id = Some(vendor);
        }
        if let Some(group_id) = input.group_id {
            product.group_id = Some(group_id);
        }
        if let Some(category_id) = input.category_id {
            product.category_id = Some(category_id);
        }
        if let Some(software_id) = input.software_id {
            product.software_id = Some(software_id);
        }
        if let Some(template) = input.license_template {
            product.license_template = template;
        }
        if let Some(department_id) = input.support_department_id {
            product.support_department_id = Some(department_id);
        }
        if let Some(department_name) = input.support_department_name {
            product.support_department_name = Some(department_name);
        }
        if product.kind == "bundle" {
            if let Some(rule) = input.bundle_rule {
                product.bundle_rule = rule;
            }
            if let Some(discount) = input.bundle_discount_bps {
                product.bundle_discount_bps = Some(discount);
            }
            if product.bundle_rule != "discount_percent" {
                product.bundle_discount_bps = None;
            }
        }
        let old_status = product.status.clone();
        let mut status_changed_to: Option<String> = None;
        if let Some(status) = input.status {
            validate_status_transition(&product.status, &status)?;
            product.status = status.clone();
            status_changed_to = Some(status);
        }
        // Leaving `active` unlists the product in the store.
        let mut delisted = false;
        if status_changed_to
            .as_deref()
            .is_some_and(|to| to != "active")
            && product.listed
        {
            product.listed = false;
            delisted = true;
        }
        let price_changed = product.price.amount_minor != old_price_minor
            || product.price.currency != old_currency;
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.updated", detail_value.clone());
        if let Some(to) = status_changed_to {
            state.emit(
                "product.status_changed",
                json!({
                    "product_id": detail.product.id,
                    "from": old_status,
                    "to": to,
                }),
            );
        }
        if price_changed {
            state.emit("product.price_changed", detail_value.clone());
        }
        if delisted {
            state.emit("product.delisted", detail_value);
        }
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_product(
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
        let id = path.as_str().to_string();
        let _product = require_product_in_scope(&state, &id, &_scope)?;
        let blocked = |what: &str| {
            UnifiedError::conflict(format!(
                "Product has {what}; delete them first"
            ))
        };
        if state
            .variants
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("variants"));
        }
        if state
            .options
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("options"));
        }
        if state
            .tiers
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("pricing tiers"));
        }
        if state
            .bundle_items
            .iter()
            .any(|entry| entry.value().bundle_product_id == id)
        {
            return Err(blocked("bundle items"));
        }
        if state
            .bundle_items
            .iter()
            .any(|entry| entry.value().child_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Product is part of another bundle; remove it from the bundle first",
            ));
        }
        if state
            .files
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("files"));
        }
        if state
            .versions
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("versions"));
        }
        if state
            .inventory
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("inventory records"));
        }
        let removed = state
            .products
            .remove(&id)
            .map(|(_, product)| product);
        match removed {
            Some(product) => {
                state.product_tags.remove(&id);
                let value = serde_json::to_value(&product).unwrap_or(Value::Null);
                state.emit("product.deleted", value);
                if product.listed {
                    state.emit("product.delisted", serde_json::to_value(&product).unwrap_or(Value::Null));
                }
                Ok(json!({ "deleted": true, "id": id }))
            }
            None => Err(UnifiedError::not_found("Product not found")),
        }
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn transition_product(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransitionProduct>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let target = body.into_inner().to;
        if !PRODUCT_STATUSES.contains(&target.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Status must be one of: {}",
                PRODUCT_STATUSES.join(", ")
            )));
        }
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        validate_status_transition(&product.status, &target)?;
        let from = product.status.clone();
        product.status = target.clone();
        let mut delisted = false;
        if target != "active" && product.listed {
            product.listed = false;
            delisted = true;
        }
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit(
            "product.status_changed",
            json!({
                "product_id": detail.product.id,
                "from": from,
                "to": target,
            }),
        );
        if delisted {
            state.emit("product.delisted", detail_value);
        }
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

/// List a product in the store: only `active` products can be
/// listed. The store consumes the `product.listed` event.
pub(crate) async fn list_product(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        if product.status != "active" {
            return Err(UnifiedError::unprocessable(
                "Only active products can be listed in the store",
            ));
        }
        if product.listed {
            return Err(UnifiedError::conflict("Product is already listed"));
        }
        product.listed = true;
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.listed", detail_value);
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

/// Remove a product from the store. The store consumes the
/// `product.delisted` event.
pub(crate) async fn delist_product(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)?;
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        if !product.listed {
            return Err(UnifiedError::conflict("Product is not listed"));
        }
        product.listed = false;
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.delisted", detail_value);
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn price_quote(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PriceQuoteRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PriceQuote> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_READ)?;
        let product = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        if let Some(region) = &body.region {
            validate_region(&region.to_uppercase())?;
        }
        state.quote_price(&product, &body.into_inner())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn get_license_template(
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
    let result: UnifiedResult<Value> = (|| {
        let product = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        Ok(json!({
            "product_id": product.id,
            "licensed": product.license_template.requires_license,
            "license_template": product.license_template,
            "seats": product.license_template.seats,
            "download_limit": product.license_template.download_limit,
        }))
    })();
    json::respond(result, ctx.is_debug())
}

/// Read the subscription plan of a product: billing period, trial,
/// renewal price, proration policy, grace period, and dunning
/// settings. This is the plan the store snapshots into a
/// subscription at checkout and billing enforces over the
/// subscription lifecycle. Non-subscription products report their
/// stored defaults with `pricing_model` unchanged, so callers can
/// tell a plan apart from a one-time price by the model.
pub(crate) async fn get_subscription_plan(
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
    let result: UnifiedResult<SubscriptionPlanView> = (|| {
        let product = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        Ok(SubscriptionPlanView {
            product_id: product.id.clone(),
            pricing_model: product.pricing_model.clone(),
            billing_period: product.billing_period.clone(),
            trial_days: product.trial_days,
            price_minor: product.price.amount_minor,
            renewal_price_minor: product
                .renewal_price
                .as_ref()
                .map(|price| price.amount_minor)
                .unwrap_or(product.price.amount_minor),
            currency: product.price.currency.clone(),
            proration_policy: product.proration_policy.clone(),
            grace_period_days: product.grace_period_days,
            dunning_max_retries: product.dunning_max_retries,
            dunning_retry_interval_days: product.dunning_retry_interval_days,
        })
    })();
    json::respond(result, ctx.is_debug())
}

/// Open a support ticket for a product. The ticket is created by the
/// tickets module through the `ticket.create_requested` hook, routed
/// to this product's support department.
pub(crate) async fn create_support_ticket(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateSupportTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRODUCTS_CREATE)?;
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Title must not be empty"));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Body must not be empty"));
        }
        // The ticket is filed as its requester: a body `user_id`
        // naming someone else is forgery unless the caller holds
        // `products:manage` (admin/on-behalf flow). The tickets
        // module records the hook payload's `user_id` as the
        // ticket owner, so the value emitted here must already be
        // the caller (or a manage-approved other).
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if user_id != caller.actor
            && !check_capability(&ctx, &caller.role, PERM_PRODUCTS_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "user_id must match the authenticated caller unless the caller holds products:manage",
            ));
        }
        let product = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_in_company_scope(&product.vendor_company_id, &_scope)?;
        if !state.bus().has("ticket.create_requested") {
            return Err(UnifiedError::unprocessable(
                "No ticket handler is registered",
            ));
        }
        let context = state.bus().emit(
            "ticket.create_requested",
            json!({
                "title": input.title,
                "body": input.body,
                "user_id": user_id,
                "company_id": product.vendor_company_id,
                "priority": input.priority,
                "category": product.slug,
                "product_id": product.id,
                "department_id": product.support_department_id,
                "department_name": product.support_department_name,
            }),
        );
        let ticket = context.results.into_iter().find(|result| {
            result.get("error").is_none()
                && (result.get("id").is_some() || result.get("ticket").is_some())
        });
        match ticket {
            Some(ticket) => Ok(json!({
                "ticket": ticket,
                "product_id": product.id,
                "department_id": product.support_department_id,
                "department_name": product.support_department_name,
            })),
            None => Err(UnifiedError::unprocessable(
                "Ticket creation was rejected by the tickets module",
            )),
        }
    })();
    json::respond(result, ctx.is_debug())
}

/// HTTP view of the `product.entitlement.check` hook: does this user
/// hold an active entitlement for this product?
pub(crate) async fn entitlement_check(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ProductsState>>,
    req: HttpRequest,
    body: web::Json<EntitlementCheckRequest>,
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
    let input = body.into_inner();
    if let Some(product) = state.products.get(&input.product_id) {
        if let Err(error) =
            require_record_in_company_scope(&product.value().vendor_company_id, &_scope)
        {
            return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
        }
    }
    let result = state.check_entitlement(&input.product_id, &input.user_id);
    json::success_response(result, 200)
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
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;
    use std::sync::Arc;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed.iter().map(|permission| permission.to_string()).collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
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
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn authed_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    #[tokio::test]
    async fn list_products_rejects_a_request_without_a_verified_caller() {
        let ctx = test_ctx(&["products:read"]);
        let state = web::Data::new(Arc::new(ProductsState::new()));
        let req = TestRequest::default().to_http_request();
        let response = list_products(
            ctx,
            state,
            req,
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn list_products_allows_a_caller_with_the_read_permission() {
        let ctx = test_ctx(&["products:read"]);
        let state = web::Data::new(Arc::new(ProductsState::new()));
        let response = list_products(
            ctx,
            state,
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn list_products_forbids_a_caller_without_the_read_permission() {
        let ctx = test_ctx(&["products:manage"]);
        let state = web::Data::new(Arc::new(ProductsState::new()));
        let response = list_products(
            ctx,
            state,
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 403);
    }

    fn ticket_body(user_id: &str) -> web::Json<CreateSupportTicket> {
        web::Json(CreateSupportTicket {
            user_id: user_id.to_string(),
            title: "Help".to_string(),
            body: "Something broke".to_string(),
            priority: None,
        })
    }

    /// A state with one product and a fake tickets listener that
    /// records the `user_id` the handler emits on
    /// `ticket.create_requested` (the id the tickets module would
    /// record as the ticket owner).
    fn ticket_state() -> (
        web::Data<Arc<ProductsState>>,
        Arc<std::sync::Mutex<Option<String>>>,
    ) {
        use super::money::Money;
        use unified_core::hooks::HookContext;
        let state = Arc::new(ProductsState::new());
        state.products.insert(
            "prod-1".to_string(),
            Product {
                id: "prod-1".to_string(),
                slug: "prod-1".to_string(),
                name: "Widget".to_string(),
                description: String::new(),
                kind: "digital".to_string(),
                status: "active".to_string(),
                pricing_model: "one_time".to_string(),
                billing_period: None,
                price: Money {
                    amount_minor: 1000,
                    currency: "USD".to_string(),
                },
                renewal_price: None,
                trial_days: 0,
                proration_policy: "prorate".to_string(),
                grace_period_days: 0,
                dunning_max_retries: 3,
                dunning_retry_interval_days: 1,
                vendor_company_id: None,
                group_id: None,
                category_id: None,
                software_id: None,
                license_template: Default::default(),
                support_department_id: Some("dept-1".to_string()),
                support_department_name: Some("Support".to_string()),
                bundle_rule: "sum".to_string(),
                bundle_discount_bps: None,
                listed: false,
                created_at: now(),
                updated_at: now(),
            },
        );
        let seen: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let seen_for_hook = seen.clone();
        state.bus().register(
            "ticket.create_requested",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let user_id = ctx
                    .get("user_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                *seen_for_hook
                    .lock()
                    .expect("seen user lock poisoned") = user_id;
                ctx.add_result(serde_json::json!({ "id": "tick-1" }));
            }),
        );
        (web::Data::new(state), seen)
    }

    #[tokio::test]
    async fn create_support_ticket_rejects_forged_requester() {
        let ctx = test_ctx(&["products:create"]);
        let state = web::Data::new(Arc::new(ProductsState::new()));
        let response = create_support_ticket(
            ctx,
            state,
            authed_request(),
            web::Path::from("prod-1".to_string()),
            ticket_body("someone-else"),
        )
        .await;
        assert_eq!(response.status().as_u16(), 403);
    }

    #[tokio::test]
    async fn create_support_ticket_files_as_the_caller() {
        let ctx = test_ctx(&["products:create"]);
        let (state, seen) = ticket_state();
        let response = create_support_ticket(
            ctx,
            state,
            authed_request(),
            web::Path::from("prod-1".to_string()),
            ticket_body("tester"),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            seen.lock().expect("seen user lock poisoned").as_deref(),
            Some("tester")
        );
    }

    #[tokio::test]
    async fn create_support_ticket_allows_manage_on_behalf() {
        let ctx = test_ctx(&["products:create", "products:manage"]);
        let (state, seen) = ticket_state();
        let response = create_support_ticket(
            ctx,
            state,
            authed_request(),
            web::Path::from("prod-1".to_string()),
            ticket_body("someone-else"),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            seen.lock().expect("seen user lock poisoned").as_deref(),
            Some("someone-else")
        );
    }
}
