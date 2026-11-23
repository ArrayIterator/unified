//! HTTP handlers for rental listings, their per-period rates,
//! availability, and the public routes.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    availability, caller, caller_company, created, err, load_listing, load_listing_by_slug,
    load_rate, ok, paginate, require_currency, require_permission, require_tenant, require_text,
    resolve_slug, resolve_tenant, validate_description, validate_late_fee_rule, validate_period,
    validate_period_label, validate_period_unit, validate_title,
};
use super::state::RentalState;
use super::types::{
    CreateListingRequest, CreateRateRequest, RentalListing, RentalRate, UpdateListingRequest,
    UpdateRateRequest, MAX_DESCRIPTION_LEN, MAX_PHOTO_IDS, MAX_TITLE_LEN, PERM_RENTAL_MANAGE,
};

/// Path carrying a listing id.
#[derive(Debug, Deserialize)]
struct ListingPath {
    listing_id: String,
}

/// Path carrying a listing id and a rate id.
#[derive(Debug, Deserialize)]
struct RatePath {
    listing_id: String,
    rate_id: String,
}

/// Query for the availability route.
#[derive(Debug, Deserialize)]
struct AvailabilityQuery {
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    #[serde(default = "default_quantity_one")]
    quantity: i64,
}

fn default_quantity_one() -> i64 {
    1
}

fn listing_view(state: &RentalState, listing: &RentalListing) -> Value {
    let mut rates: Vec<RentalRate> = state
        .rates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rate| rate.listing_id == listing.id)
        .collect();
    rates.sort_by_key(|rate| (rate.period_label.clone(), rate.price_minor));
    let mut value = serde_json::to_value(listing).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "rates".to_string(),
            serde_json::to_value(rates).unwrap_or(Value::Null),
        );
    }
    value
}

// ---------------------------------------------------------------------------
// Listing handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_listings(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("active").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<RentalListing> = state
            .listings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|listing| {
                if listing.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = &active_filter {
                    let want = active == "true";
                    if listing.active != want {
                        return false;
                    }
                }
                if let Some(product_id) = &product_filter {
                    if listing.product_id != *product_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        listing.title,
                        listing.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|listing| listing.title.clone());
        let views: Vec<Value> = items
            .iter()
            .map(|listing| listing_view(&state, listing))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_listing(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateListingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let input = body.into_inner();
        let title = validate_title(&input.title)?;
        let description = validate_description(&input.description)?;
        let slug = resolve_slug(input.slug.as_ref(), &title)?;
        let currency = require_currency(&input.currency)?;
        if input.product_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("product_id must not be empty"));
        }
        if input.deposit_amount_minor < 0 {
            return Err(UnifiedError::bad_request(
                "deposit_amount_minor must not be negative",
            ));
        }
        if input.quantity <= 0 {
            return Err(UnifiedError::bad_request("quantity must be positive"));
        }
        validate_late_fee_rule(&input.late_fee_rule)?;
        if state
            .listings
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "slug is already in use: {slug}"
            )));
        }
        let now = Utc::now();
        let listing = RentalListing {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            product_id: input.product_id.trim().to_string(),
            title,
            description,
            slug,
            currency,
            deposit_amount_minor: input.deposit_amount_minor,
            quantity: input.quantity,
            late_fee_rule: input.late_fee_rule,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.listings.insert(listing.id, listing.clone());
        state.emit(
            "rental.listing.created",
            json!({
                "tenant_id": tenant_id,
                "listing_id": listing.id.to_string(),
                "product_id": listing.product_id,
                "slug": listing.slug,
                "title": listing.title,
            }),
        );
        Ok(listing_view(&state, &listing))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_listing(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ListingPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        Ok(listing_view(&state, &listing))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_listing(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ListingPath>,
    body: web::Json<UpdateListingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let mut listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        let input = body.into_inner();
        if let Some(raw) = &input.title {
            listing.title = validate_title(raw)?;
        }
        if let Some(description) = &input.description {
            listing.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.slug {
            let slug = resolve_slug(Some(raw), &listing.title)?;
            if slug != listing.slug
                && state
                    .listings
                    .iter()
                    .any(|entry| entry.value().tenant_id == tenant_id && entry.value().slug == slug)
            {
                return Err(UnifiedError::conflict(format!(
                    "slug is already in use: {slug}"
                )));
            }
            listing.slug = slug;
        }
        if let Some(raw) = &input.currency {
            listing.currency = require_currency(raw)?;
        }
        if let Some(deposit) = input.deposit_amount_minor {
            if deposit < 0 {
                return Err(UnifiedError::bad_request(
                    "deposit_amount_minor must not be negative",
                ));
            }
            listing.deposit_amount_minor = deposit;
        }
        if let Some(quantity) = input.quantity {
            if quantity <= 0 {
                return Err(UnifiedError::bad_request("quantity must be positive"));
            }
            listing.quantity = quantity;
        }
        if let Some(rule) = &input.late_fee_rule {
            validate_late_fee_rule(rule)?;
            listing.late_fee_rule = rule.clone();
        }
        if let Some(active) = input.active {
            listing.active = active;
        }
        listing.updated_at = Utc::now();
        state.listings.insert(listing.id, listing.clone());
        state.emit(
            "rental.listing.updated",
            json!({
                "tenant_id": tenant_id,
                "listing_id": listing.id.to_string(),
                "slug": listing.slug,
                "title": listing.title,
            }),
        );
        Ok(listing_view(&state, &listing))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_listing(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ListingPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        if state
            .orders
            .iter()
            .any(|entry| entry.value().listing_id == listing.id)
        {
            return Err(UnifiedError::bad_request(
                "Listing still has orders; cancel or complete them first",
            ));
        }
        let rate_ids: Vec<Uuid> = state
            .rates
            .iter()
            .filter(|entry| entry.value().listing_id == listing.id)
            .map(|entry| *entry.key())
            .collect();
        for rate_id in rate_ids {
            state.rates.remove(&rate_id);
        }
        state.listings.remove(&listing.id);
        state.emit(
            "rental.listing.deleted",
            json!({
                "tenant_id": tenant_id,
                "listing_id": listing.id.to_string(),
                "slug": listing.slug,
                "title": listing.title,
            }),
        );
        Ok(json!({ "deleted": true, "listing_id": listing.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Rate handlers (authenticated, nested under a listing)
// ---------------------------------------------------------------------------

pub(crate) async fn list_rates(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ListingPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        let params = query.into_inner();
        let unit_filter = params.filters.get("period_unit").cloned();
        let mut items: Vec<RentalRate> = state
            .rates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rate| {
                if rate.tenant_id != tenant_id || rate.listing_id != listing.id {
                    return false;
                }
                if let Some(unit) = &unit_filter {
                    if rate.period_unit != *unit {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|rate| (rate.period_label.clone(), rate.price_minor));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_rate(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ListingPath>,
    body: web::Json<CreateRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalRate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        let input = body.into_inner();
        let period_label = validate_period_label(&input.period_label)?;
        let period_unit = validate_period_unit(&input.period_unit)?;
        validate_period(&period_unit, input.period_length)?;
        if input.price_minor < 0 {
            return Err(UnifiedError::bad_request(
                "price_minor must not be negative",
            ));
        }
        if state.rates.iter().any(|entry| {
            entry.value().listing_id == listing.id && entry.value().period_label == period_label
        }) {
            return Err(UnifiedError::conflict(format!(
                "period_label is already in use on this listing: {period_label}"
            )));
        }
        let now = Utc::now();
        let rate = RentalRate {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            listing_id: listing.id,
            period_label,
            period_unit,
            period_length: input.period_length,
            price_minor: input.price_minor,
            created_at: now,
            updated_at: now,
        };
        state.rates.insert(rate.id, rate.clone());
        state.emit(
            "rental.rate.created",
            json!({
                "tenant_id": tenant_id,
                "listing_id": listing.id.to_string(),
                "rate_id": rate.id.to_string(),
                "period_label": rate.period_label,
                "price_minor": rate.price_minor,
            }),
        );
        Ok(rate)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_rate(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<RatePath>,
    body: web::Json<UpdateRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalRate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        let mut rate = load_rate(&state, &tenant_id, &path.rate_id)?;
        if rate.listing_id != listing.id {
            return Err(UnifiedError::not_found(format!(
                "Rate not found: {}",
                rate.id
            )));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.period_label {
            let label = validate_period_label(raw)?;
            if label != rate.period_label
                && state.rates.iter().any(|entry| {
                    entry.value().listing_id == listing.id && entry.value().period_label == label
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "period_label is already in use on this listing: {label}"
                )));
            }
            rate.period_label = label;
        }
        if let Some(raw) = &input.period_unit {
            let unit = validate_period_unit(raw)?;
            validate_period(&unit, input.period_length.unwrap_or(rate.period_length))?;
            rate.period_unit = unit;
        }
        if let Some(length) = input.period_length {
            validate_period(&rate.period_unit, length)?;
            rate.period_length = length;
        }
        if let Some(price) = input.price_minor {
            if price < 0 {
                return Err(UnifiedError::bad_request(
                    "price_minor must not be negative",
                ));
            }
            rate.price_minor = price;
        }
        rate.updated_at = Utc::now();
        state.rates.insert(rate.id, rate.clone());
        state.emit(
            "rental.rate.updated",
            json!({
                "tenant_id": tenant_id,
                "listing_id": listing.id.to_string(),
                "rate_id": rate.id.to_string(),
                "period_label": rate.period_label,
            }),
        );
        Ok(rate)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_rate(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<RatePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        let rate = load_rate(&state, &tenant_id, &path.rate_id)?;
        if rate.listing_id != listing.id {
            return Err(UnifiedError::not_found(format!(
                "Rate not found: {}",
                rate.id
            )));
        }
        if state
            .orders
            .iter()
            .any(|entry| entry.value().rate_id == rate.id)
        {
            return Err(UnifiedError::bad_request(
                "Rate is used by orders; it cannot be deleted",
            ));
        }
        state.rates.remove(&rate.id);
        state.emit(
            "rental.rate.deleted",
            json!({
                "tenant_id": tenant_id,
                "listing_id": listing.id.to_string(),
                "rate_id": rate.id.to_string(),
                "period_label": rate.period_label,
            }),
        );
        Ok(json!({ "deleted": true, "rate_id": rate.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Availability (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn check_listing_availability(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ListingPath>,
    query: web::Query<AvailabilityQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let listing = load_listing(&state, &tenant_id, &path.listing_id)?;
        let input = query.into_inner();
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        if input.quantity <= 0 {
            return Err(UnifiedError::bad_request("quantity must be positive"));
        }
        Ok(availability(
            &state,
            &tenant_id,
            &listing,
            input.starts_at,
            input.ends_at,
            input.quantity,
        ))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public handlers (no caller; tenant resolution still applies)
// ---------------------------------------------------------------------------

pub(crate) async fn list_public_listings(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let params = query.into_inner();
        let product_filter = params.filters.get("product_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<RentalListing> = state
            .listings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|listing| {
                if listing.tenant_id != tenant_id || !listing.active {
                    return false;
                }
                if let Some(product_id) = &product_filter {
                    if listing.product_id != *product_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        listing.title,
                        listing.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|listing| listing.title.clone());
        let views: Vec<Value> = items
            .iter()
            .map(|listing| listing_view(&state, listing))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_public_listing(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let listing = load_listing_by_slug(&state, &tenant_id, &path.into_inner())?;
        if !listing.active {
            return Err(UnifiedError::not_found("Listing not found".to_string()));
        }
        Ok(listing_view(&state, &listing))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Validate photo id lists shared by the fulfilment handlers:
/// at most 20 ids, each non-empty.
pub(crate) fn validate_photo_ids(photo_ids: &[String]) -> UnifiedResult<Vec<String>> {
    if photo_ids.len() > MAX_PHOTO_IDS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_PHOTO_IDS} photo ids"
        )));
    }
    let mut out = Vec::with_capacity(photo_ids.len());
    for raw in photo_ids {
        out.push(require_text(raw, "photo_id", MAX_TITLE_LEN)?);
    }
    Ok(out)
}
