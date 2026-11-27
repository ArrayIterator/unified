// Store module - product, tier, and coupon handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::helpers::{
    caller_company, in_company_scope, line_total_for, matches_search, minor_to_major, paginate,
    require_caller_company, require_company_match_for, require_product_in_scope,
    require_record_in_company_scope, require_record_writable_in_company_scope, require_tenant,
    respond_created,
};
use super::pricing::{coupon_discount, resolve_tier, valid_billing_period, valid_currency, valid_kind, valid_pricing_model, valid_product_status, validate_subscription_settings};
use super::state::{StoreState, new_id, now};
use super::types::{Coupon, CreateCoupon, CreateProduct, CreateTier, PriceCalculateRequest, PricingTier, Product, UpdateCoupon, UpdateProduct, UpdateTier, default_download_limit, default_dunning_max_retries, default_dunning_retry_interval_days, default_pricing_model, default_product_status, default_proration_policy};

// Product handlers

pub(crate) async fn list_products(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "price", "created_at"], "created_at");
    let mut items: Vec<Product> = state
        .products
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|product| {
            (product.status == "published" || in_company_scope(&product.company_id, &scope))
                && matches_search(
                    &format!("{} {}", product.name, product.description),
                    &params,
                )
                && params
                .filters
                .get("kind")
                .is_none_or(|kind| product.kind == *kind)
                && params.filters.get("status").is_none_or(|status| product.status == *status)
                && params.filters.get("software_id").is_none_or(|id| {
                    product.software_id.as_deref() == Some(id.as_str())
                })
                && params.filters.get("software_slug").is_none_or(|slug| {
                    product.software_slug.as_deref() == Some(slug.as_str())
                })
                && params.filters.get("pricing_model").is_none_or(|model| {
                    product.pricing_model == *model
                })
                && params.filters.get("company_id").is_none_or(|company_id| {
                    product.company_id.as_deref() == Some(company_id.as_str())
                })
                && params
                    .filters
                    .get("currency")
                    .is_none_or(|currency| product.currency.eq_ignore_ascii_case(currency))
                && params.filters.get("license_product").is_none_or(|flag| {
                    product.license_product == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "price" => items.sort_by(|a, b| a.price.cmp(&b.price)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) fn validate_product_pricing(input_pricing_model: Option<&str>, input_billing_period: Option<&str>) -> UnifiedResult<()> {
    if let Some(model) = input_pricing_model {
        if !valid_pricing_model(model) {
            return Err(UnifiedError::bad_request(
                "Pricing model must be one of: one_time, subscription",
            ));
        }
    }
    if let Some(period) = input_billing_period {
        if !valid_billing_period(period) {
            return Err(UnifiedError::bad_request(
                "Billing period must be one of: monthly, yearly",
            ));
        }
    }
    Ok(())
}

pub(crate) async fn create_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Product name must not be empty"));
        }
        if input.price < 0 {
            return Err(UnifiedError::bad_request(
                "Product price must be a non-negative number",
            ));
        }
        if !valid_kind(&input.kind) {
            return Err(UnifiedError::bad_request(
                "Product kind must be one of: theme, plugin, software",
            ));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "Currency must be a 3-letter code such as USD",
            ));
        }
        if let Some(status) = &input.status {
            if !valid_product_status(status) {
                return Err(UnifiedError::bad_request(
                    "Product status must be one of: draft, published",
                ));
            }
        }
        validate_product_pricing(
            input.pricing_model.as_deref(),
            input.billing_period.as_deref(),
        )?;
        if let Some(renewal_price) = input.renewal_price {
            if renewal_price < 0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
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
        validate_subscription_settings(
            input.proration_policy.as_deref(),
            input.grace_period_days,
            input.dunning_max_retries,
            input.dunning_retry_interval_days,
        )?;
        let pricing_model = input
            .pricing_model
            .unwrap_or_else(default_pricing_model);
        if pricing_model == "subscription" && input.billing_period.is_none() {
            return Err(UnifiedError::bad_request(
                "Subscription products require a billing period",
            ));
        }
        let company_id = require_company_match_for(&scope, &input.company_id)?;
        let product = Product {
            id: new_id(),
            name: input.name.trim().to_string(),
            description: input.description.unwrap_or_default(),
            price: input.price,
            currency: currency.to_uppercase(),
            kind: input.kind,
            // A product linked to software is a license product unless
            // the caller explicitly says otherwise.
            license_product: input
                .license_product
                .unwrap_or_else(|| input.software_id.is_some()),
            file_id: input.file_id,
            slug: input.slug,
            software_id: input.software_id,
            software_slug: input.software_slug,
            status: input.status.unwrap_or_else(default_product_status),
            company_id,
            pricing_model,
            billing_period: input.billing_period,
            renewal_price: input.renewal_price,
            trial_days: input.trial_days.unwrap_or(0),
            proration_policy: input
                .proration_policy
                .unwrap_or_else(default_proration_policy),
            grace_period_days: input.grace_period_days.unwrap_or(0),
            dunning_max_retries: input
                .dunning_max_retries
                .unwrap_or_else(default_dunning_max_retries),
            dunning_retry_interval_days: input
                .dunning_retry_interval_days
                .unwrap_or_else(default_dunning_retry_interval_days),
            module_id: input.module_id,
            updates_years: input.updates_years,
            created_at: now(),
            updated_at: now(),
        };
        state.products.insert(product.id.clone(), product.clone());
        Ok(product)
    })();
    respond_created(result)
}

pub(crate) async fn get_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Product> = (|| {
        let product = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.status != "published" && !in_company_scope(&product.company_id, &scope) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        Ok(product)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        // Validate pricing before touching the stored product so a
        // rejected update never leaves a half-mutated record behind.
        validate_product_pricing(
            input.pricing_model.as_deref(),
            input.billing_period.as_deref(),
        )?;
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_writable_in_company_scope(&product.company_id, &scope)?;
        require_company_match_for(&scope, &input.company_id)?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Product name must not be empty"));
            }
            product.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            product.description = description;
        }
        if let Some(price) = input.price {
            if price < 0 {
                return Err(UnifiedError::bad_request(
                    "Product price must be a non-negative number",
                ));
            }
            product.price = price;
        }
        if let Some(currency) = input.currency {
            if !valid_currency(&currency) {
                return Err(UnifiedError::bad_request(
                    "Currency must be a 3-letter code such as USD",
                ));
            }
            product.currency = currency.to_uppercase();
        }
        if let Some(kind) = input.kind {
            if !valid_kind(&kind) {
                return Err(UnifiedError::bad_request(
                    "Product kind must be one of: theme, plugin, software",
                ));
            }
            product.kind = kind;
        }
        if let Some(license_product) = input.license_product {
            product.license_product = license_product;
        }
        if let Some(file_id) = input.file_id {
            product.file_id = Some(file_id);
        }
        if let Some(slug) = input.slug {
            product.slug = Some(slug);
        }
        if let Some(software_id) = input.software_id {
            product.software_id = Some(software_id);
        }
        if let Some(software_slug) = input.software_slug {
            product.software_slug = Some(software_slug);
        }
        if let Some(status) = input.status {
            if !valid_product_status(&status) {
                return Err(UnifiedError::bad_request(
                    "Product status must be one of: draft, published",
                ));
            }
            product.status = status;
        }
        if let Some(company_id) = input.company_id {
            product.company_id = Some(company_id);
        }
        if let Some(pricing_model) = input.pricing_model {
            product.pricing_model = pricing_model;
        }
        if let Some(billing_period) = input.billing_period {
            product.billing_period = Some(billing_period);
        }
        if let Some(renewal_price) = input.renewal_price {
            if renewal_price < 0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
                ));
            }
            product.renewal_price = Some(renewal_price);
        }
        if let Some(trial_days) = input.trial_days {
            product.trial_days = trial_days;
        }
        if let Some(proration_policy) = input.proration_policy {
            if !matches!(
                proration_policy.as_str(),
                "prorate" | "no_prorate" | "credit_only"
            ) {
                return Err(UnifiedError::bad_request(
                    "Proration policy must be one of: prorate, no_prorate, credit_only",
                ));
            }
            product.proration_policy = proration_policy;
        }
        if let Some(grace_period_days) = input.grace_period_days {
            if grace_period_days > 90 {
                return Err(UnifiedError::bad_request(
                    "Grace period must be at most 90 days",
                ));
            }
            product.grace_period_days = grace_period_days;
        }
        if let Some(dunning_max_retries) = input.dunning_max_retries {
            if dunning_max_retries > 10 {
                return Err(UnifiedError::bad_request(
                    "Dunning max retries must be at most 10",
                ));
            }
            product.dunning_max_retries = dunning_max_retries;
        }
        if let Some(dunning_retry_interval_days) = input.dunning_retry_interval_days {
            if dunning_retry_interval_days == 0 || dunning_retry_interval_days > 30 {
                return Err(UnifiedError::bad_request(
                    "Dunning retry interval must be between 1 and 30 days",
                ));
            }
            product.dunning_retry_interval_days = dunning_retry_interval_days;
        }
        if let Some(module_id) = input.module_id {
            if module_id.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Module id must not be empty",
                ));
            }
            product.module_id = Some(module_id.trim().to_string());
        }
        if let Some(updates_years) = input.updates_years {
            product.updates_years = Some(updates_years);
        }
        product.updated_at = now();
        Ok(product.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let id = path.as_str().to_string();
        let product = state
            .products
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_writable_in_company_scope(&product.company_id, &scope)?;
        let has_tiers = state.tiers.iter().any(|entry| entry.value().product_id == id);
        if has_tiers {
            return Err(UnifiedError::conflict(
                "Product has pricing tiers; delete the tiers first",
            ));
        }
        state
            .products
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Product not found"))
    })();
    json::respond(result, false)
}

pub(crate) async fn publish_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_writable_in_company_scope(&product.company_id, &scope)?;
        if product.status == "published" {
            return Err(UnifiedError::conflict("Product is already published"));
        }
        product.status = "published".to_string();
        product.updated_at = now();
        let value = serde_json::to_value(&*product).unwrap_or(Value::Null);
        drop(product);
        state.emit("store.product.published", value);
        Ok(state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("product still present"))
    })();
    json::respond(result, false)
}

pub(crate) async fn unpublish_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        require_record_writable_in_company_scope(&product.company_id, &scope)?;
        if product.status == "draft" {
            return Err(UnifiedError::conflict("Product is already a draft"));
        }
        product.status = "draft".to_string();
        product.updated_at = now();
        let value = serde_json::to_value(&*product).unwrap_or(Value::Null);
        drop(product);
        state.emit("store.product.unpublished", value);
        Ok(state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("product still present"))
    })();
    json::respond(result, false)
}

// Pricing tier handlers

pub(crate) async fn list_tiers(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let product_id = path.into_inner();
    let product = match state
        .products
        .get(&product_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Product not found"))
    {
        Ok(product) => product,
        Err(error) => return json::error_response(&error, false),
    };
    if product.status != "published" && !in_company_scope(&product.company_id, &scope) {
        return json::error_response(&UnifiedError::not_found("Product not found"), false);
    }
    let mut items: Vec<PricingTier> = state
        .tiers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tier| {
            tier.product_id == product_id
                && matches_search(&format!("{} {}", tier.slug, tier.label), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| tier.status == *status)
                && params
                    .filters
                    .get("slug")
                    .is_none_or(|slug| tier.slug == *slug)
        })
        .collect();
    items.sort_by(|a, b| a.price.cmp(&b.price));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_tier(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateTier>,
) -> HttpResponse {
    let result: UnifiedResult<PricingTier> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let product_id = path.into_inner();
        let _product = require_product_in_scope(&state, &scope, &product_id)?;
        let input = body.into_inner();
        if input.slug.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tier slug must not be empty"));
        }
        if input.price < 0 {
            return Err(UnifiedError::bad_request(
                "Tier price must be a non-negative number",
            ));
        }
        if let Some(billing_period) = &input.billing_period {
            if !valid_billing_period(billing_period) {
                return Err(UnifiedError::bad_request(
                    "Billing period must be one of: monthly, yearly",
                ));
            }
        }
        if let Some(renewal_price) = input.renewal_price {
            if renewal_price < 0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
                ));
            }
        }
        let seats = input.seats.unwrap_or(1);
        if seats < 1 {
            return Err(UnifiedError::bad_request("Tier seats must be at least 1"));
        }
        let slug = input.slug.trim().to_string();
        let duplicate = state
            .tiers
            .iter()
            .any(|entry| entry.value().product_id == product_id && entry.value().slug == slug);
        if duplicate {
            return Err(UnifiedError::conflict(
                "A tier with this slug already exists for the product",
            ));
        }
        let tier = PricingTier {
            id: new_id(),
            product_id,
            slug,
            label: input.label.unwrap_or_else(|| input.slug.trim().to_string()),
            price: input.price,
            renewal_price: input.renewal_price,
            billing_period: input.billing_period,
            trial_days: input.trial_days.unwrap_or(0),
            seats,
            download_limit: input.download_limit.unwrap_or(default_download_limit()),
            features: input.features.unwrap_or_default(),
            status: input.status.unwrap_or_else(|| "active".to_string()),
            created_at: now(),
            updated_at: now(),
        };
        if tier.status != "active" && tier.status != "disabled" {
            return Err(UnifiedError::bad_request(
                "Tier status must be one of: active, disabled",
            ));
        }
        state.tiers.insert(tier.id.clone(), tier.clone());
        Ok(tier)
    })();
    respond_created(result)
}

pub(crate) async fn get_tier(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<PricingTier> = (|| {
        let tier = state
            .tiers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        let product = state
            .products
            .get(&tier.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        if product.status != "published" && !in_company_scope(&product.company_id, &scope) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        Ok(tier)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_tier(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTier>,
) -> HttpResponse {
    let result: UnifiedResult<PricingTier> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        let mut tier = state
            .tiers
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        let product_id = tier.product_id.clone();
        let _product = require_product_in_scope(&state, &scope, &product_id)?;
        if let Some(slug) = input.slug {
            if slug.trim().is_empty() {
                return Err(UnifiedError::bad_request("Tier slug must not be empty"));
            }
            tier.slug = slug.trim().to_string();
        }
        if let Some(label) = input.label {
            tier.label = label;
        }
        if let Some(price) = input.price {
            if price < 0 {
                return Err(UnifiedError::bad_request(
                    "Tier price must be a non-negative number",
                ));
            }
            tier.price = price;
        }
        if let Some(renewal_price) = input.renewal_price {
            if renewal_price < 0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
                ));
            }
            tier.renewal_price = Some(renewal_price);
        }
        if let Some(billing_period) = input.billing_period {
            if !valid_billing_period(&billing_period) {
                return Err(UnifiedError::bad_request(
                    "Billing period must be one of: monthly, yearly",
                ));
            }
            tier.billing_period = Some(billing_period);
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
        if let Some(download_limit) = input.download_limit {
            tier.download_limit = download_limit;
        }
        if let Some(features) = input.features {
            tier.features = features;
        }
        if let Some(status) = input.status {
            if status != "active" && status != "disabled" {
                return Err(UnifiedError::bad_request(
                    "Tier status must be one of: active, disabled",
                ));
            }
            tier.status = status;
        }
        tier.updated_at = now();
        Ok(tier.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_tier(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let tier = state
            .tiers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        let _product = require_product_in_scope(&state, &scope, &tier.product_id)?;
        state
            .tiers
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))
    })();
    json::respond(result, false)
}

// Coupon handlers and price calculation

pub(crate) async fn list_coupons(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = caller_company(&tenant, &req) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let mut items: Vec<Coupon> = state
        .coupons
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|coupon| {
            matches_search(&coupon.code, &params)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|flag| coupon.active == (*flag == "true" || *flag == "1"))
                && params.filters.get("discount_type").is_none_or(|kind| {
                    coupon.discount_type == *kind
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_coupon(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateCoupon>,
) -> HttpResponse {
    let result: UnifiedResult<Coupon> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        let code = input.code.trim().to_uppercase();
        if code.is_empty() {
            return Err(UnifiedError::bad_request("Coupon code must not be empty"));
        }
        if input.discount_type != "percent" && input.discount_type != "fixed" {
            return Err(UnifiedError::bad_request(
                "Discount type must be one of: percent, fixed",
            ));
        }
        if input.discount_type == "percent" {
            if input.discount_percent_bp <= 0 || input.discount_percent_bp > 10_000 {
                return Err(UnifiedError::bad_request(
                    "Percentage discount must be between 1 and 10000 basis points (0.01% to 100%)",
                ));
            }
            if input.discount_fixed_minor != 0 {
                return Err(UnifiedError::bad_request(
                    "A percent coupon must not carry a fixed discount",
                ));
            }
        } else if input.discount_fixed_minor <= 0 {
            return Err(UnifiedError::bad_request(
                "Fixed discount must be greater than zero minor units",
            ));
        } else if input.discount_percent_bp != 0 {
            return Err(UnifiedError::bad_request(
                "A fixed coupon must not carry a percent discount",
            ));
        }
        if let Some(expires_at) = input.expires_at {
            if expires_at <= now() {
                return Err(UnifiedError::bad_request(
                    "Coupon expiry must be in the future",
                ));
            }
        }
        if let Some(limit) = input.usage_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "Coupon usage limit must be at least 1",
                ));
            }
        }
        let product_ids = input.product_ids.unwrap_or_default();
        for product_id in &product_ids {
            let _product = require_product_in_scope(&state, &scope, product_id)?;
        }
        if state
            .coupons
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(
                "A coupon with this code already exists",
            ));
        }
        let coupon = Coupon {
            id: new_id(),
            code,
            discount_type: input.discount_type,
            discount_percent_bp: input.discount_percent_bp,
            discount_fixed_minor: input.discount_fixed_minor,
            expires_at: input.expires_at,
            usage_limit: input.usage_limit,
            used_count: 0,
            product_ids,
            active: input.active.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.coupons.insert(coupon.id.clone(), coupon.clone());
        Ok(coupon)
    })();
    respond_created(result)
}

pub(crate) async fn get_coupon(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = caller_company(&tenant, &req) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Coupon> = state
        .coupons
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Coupon not found"));
    json::respond(result, false)
}

pub(crate) async fn update_coupon(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCoupon>,
) -> HttpResponse {
    let result: UnifiedResult<Coupon> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        let mut coupon = state
            .coupons
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
        if let Some(discount_type) = input.discount_type {
            if discount_type != "percent" && discount_type != "fixed" {
                return Err(UnifiedError::bad_request(
                    "Discount type must be one of: percent, fixed",
                ));
            }
            coupon.discount_type = discount_type;
        }
        if let Some(discount_percent_bp) = input.discount_percent_bp {
            if coupon.discount_type == "fixed" && discount_percent_bp != 0 {
                return Err(UnifiedError::bad_request(
                    "A fixed coupon must not carry a percent discount",
                ));
            }
            if coupon.discount_type == "percent"
                && (discount_percent_bp <= 0 || discount_percent_bp > 10_000)
            {
                return Err(UnifiedError::bad_request(
                    "Percentage discount must be between 1 and 10000 basis points (0.01% to 100%)",
                ));
            }
            coupon.discount_percent_bp = discount_percent_bp;
        }
        if let Some(discount_fixed_minor) = input.discount_fixed_minor {
            if coupon.discount_type == "percent" && discount_fixed_minor != 0 {
                return Err(UnifiedError::bad_request(
                    "A percent coupon must not carry a fixed discount",
                ));
            }
            if coupon.discount_type == "fixed" && discount_fixed_minor <= 0 {
                return Err(UnifiedError::bad_request(
                    "Fixed discount must be greater than zero minor units",
                ));
            }
            coupon.discount_fixed_minor = discount_fixed_minor;
        }
        if let Some(expires_at) = input.expires_at {
            coupon.expires_at = Some(expires_at);
        }
        if let Some(usage_limit) = input.usage_limit {
            if usage_limit == 0 {
                return Err(UnifiedError::bad_request(
                    "Coupon usage limit must be at least 1",
                ));
            }
            coupon.usage_limit = Some(usage_limit);
        }
        if let Some(product_ids) = input.product_ids {
            for product_id in &product_ids {
                let _product = require_product_in_scope(&state, &scope, product_id)?;
            }
            coupon.product_ids = product_ids;
        }
        if let Some(active) = input.active {
            coupon.active = active;
        }
        coupon.updated_at = now();
        Ok(coupon.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_coupon(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        state
            .coupons
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))
    })();
    json::respond(result, false)
}

/// Calculate the price of a product (optionally a tier and a coupon)
/// without creating an order. The coupon is applied honestly: the
/// response shows the subtotal, the discount, and the final total.
pub(crate) async fn calculate_price(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<PriceCalculateRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        let quantity = input.quantity.unwrap_or(1);
        if quantity == 0 {
            return Err(UnifiedError::bad_request("Quantity must be at least 1"));
        }
        let product = state
            .products
            .get(&input.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.status != "published" && !in_company_scope(&product.company_id, &scope) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        let (unit_price, tier_id, tier_name, seats, trial_days, download_limit, billing_period, renewal_price) =
            match &input.tier_id {
                Some(tier_id) => {
                    let tier = resolve_tier(&state, &product.id, tier_id)?;
                    (
                        tier.price,
                        Some(tier.id.clone()),
                        Some(tier.label.clone()),
                        tier.seats,
                        tier.trial_days,
                        tier.download_limit,
                        tier.billing_period
                            .or_else(|| product.billing_period.clone()),
                        tier.renewal_price.or(product.renewal_price),
                    )
                }
                None => (
                    product.price,
                    None,
                    None,
                    1,
                    product.trial_days,
                    default_download_limit(),
                    product.billing_period.clone(),
                    product.renewal_price,
                ),
            };
        let subtotal = line_total_for(unit_price, quantity);
        let (discount, coupon_value) = match &input.coupon_code {
            Some(code) if !code.trim().is_empty() => {
                let (coupon, discount) =
                    coupon_discount(&state, code, &product.id, subtotal, &product.currency)?;
                (
                    discount,
                    Some(json!({
                        "code": coupon.code,
                        "discount_type": coupon.discount_type,
                        "discount_percent_bp": coupon.discount_percent_bp,
                        "discount_fixed_minor": coupon.discount_fixed_minor,
                    })),
                )
            }
            _ => (0, None),
        };
        let renewal_price = renewal_price.map(|price| minor_to_major(price, &product.currency));
        Ok(json!({
            "product_id": product.id,
            "product_name": product.name,
            "tier_id": tier_id,
            "tier_name": tier_name,
            "unit_price": minor_to_major(unit_price, &product.currency),
            "quantity": quantity,
            "subtotal": minor_to_major(subtotal, &product.currency),
            "discount": minor_to_major(discount, &product.currency),
            "total": minor_to_major(subtotal - discount, &product.currency),
            "currency": product.currency,
            "coupon": coupon_value,
            "pricing_model": product.pricing_model,
            "billing_period": billing_period,
            "renewal_price": renewal_price,
            "trial_days": trial_days,
            "seats": seats,
            "download_limit": download_limit,
        }))
    })();
    json::respond(result, false)
}
