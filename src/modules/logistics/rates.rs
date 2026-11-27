//! Logistics module - shipments, fleet, warehouses, delivery - Rate table + quote handlers.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::helpers::*;
use super::hooks::*;
use super::module::*;
use super::requests::*;
use super::shipments::*;
use super::state::*;
use super::tracking::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_rate_tables(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let origin_filter = params.filters.get("origin_zone").cloned();
        let dest_filter = params.filters.get("destination_zone").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<RateTable> = state
            .rate_tables
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|table| in_company_scope(&table.company_id, &_scope))
            .filter(|table| {
                if let Some(origin) = &origin_filter {
                    if table.origin_zone.as_deref() != Some(origin.as_str()) {
                        return false;
                    }
                }
                if let Some(dest) = &dest_filter {
                    if table.destination_zone.as_deref() != Some(dest.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let wanted = active == "true";
                    if table.active != wanted {
                        return false;
                    }
                }
                if !search.is_empty() && !table.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateRateTableRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RateTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let origin_zone = optional_text(&input.origin_zone, "origin_zone", MAX_ZONE_LEN)?;
        let destination_zone =
            optional_text(&input.destination_zone, "destination_zone", MAX_ZONE_LEN)?;
        let base_price = valid_money(input.base_price, "base_price")?;
        let per_kg = valid_money(input.per_kg, "per_kg")?;
        let per_km = valid_money(input.per_km, "per_km")?;
        let currency = input
            .currency
            .as_deref()
            .map(valid_currency)
            .transpose()?
            .unwrap_or_else(|| "USD".to_string());
        let company_id =
            parse_optional_company(&require_company_match(&_tenant, &req, &input.company_id)?)?;
        let now = Utc::now();
        let table = RateTable {
            id: Uuid::now_v7(),
            name,
            origin_zone,
            destination_zone,
            base_price,
            per_kg,
            per_km,
            currency,
            active: input.active.unwrap_or(true),
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.rate_tables.insert(table.id, table.clone());
        state.emit(
            "logistics.rate_table.created",
            json!({
                "rate_table_id": table.id.to_string(),
                "name": table.name,
                "created_by": caller.actor,
            }),
        );
        Ok(table)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RateTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let table = load_rate_table(&state, &path)?;
        require_record_in_company_scope(&table.company_id, &_scope)?;
        require_record_in_company_scope(&table.company_id, &_scope)?;
        Ok(table)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRateTableRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RateTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut table = load_rate_table(&state, &path)?;
        require_record_writable_in_company_scope(&table.company_id, &_scope)?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            table.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(origin) = &input.origin_zone {
            table.origin_zone = match origin.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "origin_zone", MAX_ZONE_LEN)?),
            };
        }
        if let Some(destination) = &input.destination_zone {
            table.destination_zone = match destination.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "destination_zone", MAX_ZONE_LEN)?),
            };
        }
        if let Some(base_price) = input.base_price {
            table.base_price = valid_money(base_price, "base_price")?;
        }
        if let Some(per_kg) = input.per_kg {
            table.per_kg = valid_money(per_kg, "per_kg")?;
        }
        if let Some(per_km) = input.per_km {
            table.per_km = valid_money(per_km, "per_km")?;
        }
        if let Some(currency) = &input.currency {
            table.currency = valid_currency(currency)?;
        }
        if let Some(active) = input.active {
            table.active = active;
        }
        if let Some(company_id) = &input.company_id {
            table.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => {
                    require_company_match_for(&_scope, &Some(raw.to_string()))?;
                    Some(parse_uuid(raw, "company")?)
                }
            };
        }
        table.updated_at = Utc::now();
        state.rate_tables.insert(table.id, table.clone());
        state.emit(
            "logistics.rate_table.updated",
            json!({
                "rate_table_id": table.id.to_string(),
                "name": table.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(table)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let table = load_rate_table(&state, &path)?;
        require_record_writable_in_company_scope(&table.company_id, &_scope)?;
        state.rate_tables.remove(&table.id);
        state.emit(
            "logistics.rate_table.deleted",
            json!({
                "rate_table_id": table.id.to_string(),
                "name": table.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "rate_table_id": table.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Pick the best matching active rate table: an exact zone pair
/// first, then a table matching one zone, then the wildcard table
/// (both zones unset). Returns `None` when no table matches.
pub(super) fn find_rate_table(
    state: &LogisticsState,
    origin: &str,
    destination: &str,
) -> Option<RateTable> {
    let mut tables: Vec<RateTable> = state
        .rate_tables
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|table| table.active)
        .collect();
    tables.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let score = |table: &RateTable| -> i64 {
        let origin_match = table.origin_zone.as_deref() == Some(origin);
        let dest_match = table.destination_zone.as_deref() == Some(destination);
        match (origin_match, dest_match) {
            (true, true) => 3,
            (true, false) | (false, true) => {
                if table.origin_zone.is_none() || table.destination_zone.is_none() {
                    2
                } else {
                    -1
                }
            }
            (false, false) => {
                if table.origin_zone.is_none() && table.destination_zone.is_none() {
                    1
                } else {
                    -1
                }
            }
        }
    };
    tables
        .into_iter()
        .filter(|table| score(table) >= 0)
        .max_by_key(score)
}

pub(super) async fn quote_shipping(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<QuoteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_QUOTE)?;
        let input = body.into_inner();
        let zone_origin = require_text(&input.zone_origin, "zone_origin", MAX_ZONE_LEN)?;
        let zone_destination =
            require_text(&input.zone_destination, "zone_destination", MAX_ZONE_LEN)?;
        validate_dimensions(input.weight_kg, input.length_cm, input.width_cm, input.height_cm)?;
        if let Some(latitude) = input.origin_latitude {
            validate_latitude(latitude, "origin_latitude")?;
        }
        if let Some(longitude) = input.origin_longitude {
            validate_longitude(longitude, "origin_longitude")?;
        }
        if let Some(latitude) = input.destination_latitude {
            validate_latitude(latitude, "destination_latitude")?;
        }
        if let Some(longitude) = input.destination_longitude {
            validate_longitude(longitude, "destination_longitude")?;
        }
        let currency = input
            .currency
            .as_deref()
            .map(valid_currency)
            .transpose()?
            .unwrap_or_else(|| "USD".to_string());
        let table = match input.rate_table_id {
            Some(id) => {
                let table = load_rate_table(&state, &id.to_string())?;
                if !table.active {
                    return Err(UnifiedError::conflict(
                        "The requested rate table is inactive",
                    ));
                }
                table
            }
            None => find_rate_table(&state, &zone_origin, &zone_destination).ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No active rate table matches zones \"{zone_origin}\" -> \"{zone_destination}\""
                ))
            })?,
        };
        // Distance: caller-supplied wins; otherwise ask the
        // Calculator module through the distance query (see the
        // distance note in the module header). No distance is ever
        // guessed - an unanswered query fails the quote.
        let mut distance_km = input.distance_km;
        let mut distance_source = "caller";
        if let Some(km) = distance_km {
            if !km.is_finite() || km < 0.0 {
                return Err(UnifiedError::bad_request("distance_km must be >= 0"));
            }
        } else if let (Some(o_lat), Some(o_lng), Some(d_lat), Some(d_lng)) = (
            input.origin_latitude,
            input.origin_longitude,
            input.destination_latitude,
            input.destination_longitude,
        ) {
            match request_distance(&ctx, o_lat, o_lng, d_lat, d_lng) {
                Some(km) => {
                    distance_km = Some(km);
                    distance_source = "calculator.distance_requested";
                }
                None => {
                    return Err(UnifiedError::unprocessable(
                        "No distance_km was supplied and no module answered the calculator.distance_requested query; supply distance_km or enable a distance provider",
                    ));
                }
            }
        } else {
            return Err(UnifiedError::bad_request(
                "distance_km is required when origin/destination coordinates are not both supplied",
            ));
        }
        let distance_km = distance_km.expect("distance resolved above");
        let dimensional = dimensional_weight(input.length_cm, input.width_cm, input.height_cm);
        let billable_kg = input.weight_kg.max(dimensional);
        let total = round_money(
            table.base_price + table.per_kg * billable_kg + table.per_km * distance_km,
        );
        state.emit(
            "logistics.quote.created",
            json!({
                "rate_table_id": table.id.to_string(),
                "zone_origin": zone_origin,
                "zone_destination": zone_destination,
                "billable_kg": billable_kg,
                "distance_km": distance_km,
                "distance_source": distance_source,
                "total": total,
                "currency": currency,
                "quoted_by": caller.actor,
            }),
        );
        Ok(json!({
            "rate_table_id": table.id.to_string(),
            "rate_table_name": table.name,
            "zone_origin": zone_origin,
            "zone_destination": zone_destination,
            "actual_weight_kg": input.weight_kg,
            "dimensional_weight_kg": round_money(dimensional),
            "billable_weight_kg": round_money(billable_kg),
            "distance_km": distance_km,
            "distance_source": distance_source,
            "base_price": table.base_price,
            "per_kg": table.per_kg,
            "per_km": table.per_km,
            "total": total,
            "currency": currency,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
