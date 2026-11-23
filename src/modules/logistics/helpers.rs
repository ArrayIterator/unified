//! Logistics module — shipments, fleet, warehouses, delivery — Helpers.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::hooks::*;
use super::module::*;
use super::rates::*;
use super::requests::*;
use super::shipments::*;
use super::state::*;
use super::tracking::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hardening::{trusted_client_ip_for_http_request, SecuritySettings};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(super) struct Caller {
    pub(super) actor: String,
    pub(super) role: String,
}

pub(super) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Note: logistics records carry `company_id: Option<Uuid>`, not
/// `Option<String>`, so the scope helpers below compare against
/// the caller's company parsed as a UUID.
pub(super) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(super) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Resolve the caller's company for a WRITE (C3 task 315, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3 task 315):
/// a write requires a resolved company scope.
pub(super) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(super) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller)) => Ok(Some(caller.clone())),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records carrying
/// exactly that company match, plus unscoped (global/legacy) records
/// (C3 task 320, rule A); an unresolved caller still sees global rows only.
/// A caller scope that is not a UUID matches nothing (fail
/// closed): logistics company ids are UUIDs.
pub(super) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => match Uuid::parse_str(company) {
            Ok(scope) => record_company_id.is_none() || record_company_id == &Some(scope),
            Err(_) => false,
        },
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(super) fn require_record_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3 task 315, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records — global (unscoped) records are platform-only
/// — and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(super) fn require_record_writable_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) => match Uuid::parse_str(caller) {
            Ok(scope) if *record == scope => Ok(()),
            _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
        },
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(super) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(super) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// A driver may operate a shipment (transition, proof) when they
/// hold `logistics.driver` and are the shipment's assigned driver;
/// a manager may always operate it. Anyone else is rejected. The
/// caller's driver row is resolved by their user id, so a caller
/// cannot claim another driver's shipment by guessing a driver id.
pub(super) fn can_operate_shipment(
    ctx: &AppContext,
    state: &LogisticsState,
    caller: &Caller,
    shipment: &Shipment,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_LOGISTICS_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_LOGISTICS_DRIVER) {
        let assigned = shipment.driver_id.is_some_and(|driver_id| {
            state
                .drivers
                .get(&driver_id)
                .is_some_and(|entry| entry.value().user_id == caller.actor)
        });
        if assigned {
            return Ok(());
        }
    }
    Err(UnifiedError::forbidden(
        "Caller may not operate this shipment",
    ))
}

pub(super) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(super) fn parse_optional_company(raw: &Option<String>) -> UnifiedResult<Option<Uuid>> {
    match raw {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => Ok(None),
        Some(value) => Ok(Some(parse_uuid(value, "company")?)),
    }
}

pub(super) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > max {
                Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
    }
}

pub(super) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(super) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(super) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(super) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Money: finite, non-negative, rounded to 4 decimal places.
pub(super) fn valid_money(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number >= 0"
        )));
    }
    Ok(round_money(value))
}

pub(super) fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

pub(super) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != MAX_CURRENCY_LEN || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

pub(super) fn validate_latitude(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(-90.0..=90.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between -90 and 90"
        )));
    }
    Ok(())
}

pub(super) fn validate_longitude(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(-180.0..=180.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between -180 and 180"
        )));
    }
    Ok(())
}

pub(super) fn validate_endpoint(
    endpoint: &ShipmentEndpoint,
    field: &str,
) -> UnifiedResult<ShipmentEndpoint> {
    let address = require_text(&endpoint.address, &format!("{field}.address"), MAX_ADDRESS_LEN)?;
    if let Some(latitude) = endpoint.latitude {
        validate_latitude(latitude, &format!("{field}.latitude"))?;
    }
    if let Some(longitude) = endpoint.longitude {
        validate_longitude(longitude, &format!("{field}.longitude"))?;
    }
    Ok(ShipmentEndpoint {
        address,
        latitude: endpoint.latitude,
        longitude: endpoint.longitude,
    })
}

pub(super) fn validate_dimensions(
    weight_kg: f64,
    length: f64,
    width: f64,
    height: f64,
) -> UnifiedResult<()> {
    if !weight_kg.is_finite() || weight_kg <= 0.0 || weight_kg > MAX_WEIGHT_KG {
        return Err(UnifiedError::bad_request(format!(
            "weight_kg must be > 0 and <= {MAX_WEIGHT_KG}"
        )));
    }
    for (value, name) in [
        (length, "length_cm"),
        (width, "width_cm"),
        (height, "height_cm"),
    ] {
        if !value.is_finite() || value <= 0.0 || value > MAX_DIMENSION_CM {
            return Err(UnifiedError::bad_request(format!(
                "{name} must be > 0 and <= {MAX_DIMENSION_CM}"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_window(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if let (Some(start), Some(end)) = (start, end) {
        if end <= start {
            return Err(UnifiedError::bad_request(
                "window_end must be after window_start",
            ));
        }
    }
    Ok(())
}

pub(super) fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Luhn check digit over a digit string: double every second digit
/// from the right, sum the digits, and return the digit that makes
/// the total a multiple of 10.
pub(super) fn luhn_check_digit(digits: &str) -> char {
    let mut sum: u32 = 0;
    let mut double = true;
    for ch in digits.chars().rev() {
        let mut digit = ch.to_digit(10).unwrap_or(0);
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    let check = (10 - (sum % 10)) % 10;
    char::from_digit(check, 10).unwrap_or('0')
}

/// Generate a tracking number `LG-<8 digits>-<check digit>`,
/// unique against the shipments already stored. Collisions
/// regenerate the digits; the loop is bounded so a pathological
/// store cannot hang the request.
pub(super) fn generate_tracking_number(state: &LogisticsState) -> UnifiedResult<String> {
    let mut rng = rand::rng();
    for _ in 0..100 {
        let digits: String = (0..8)
            .map(|_| rng.random_range(0..10).to_string())
            .collect();
        let check = luhn_check_digit(&digits);
        let candidate = format!("LG-{digits}-{check}");
        if !state
            .shipments
            .iter()
            .any(|entry| entry.value().tracking_number == candidate)
        {
            return Ok(candidate);
        }
    }
    Err(UnifiedError::internal(
        "Could not generate a unique tracking number",
    ))
}

/// Generate a 32-character hex verification token for public
/// tracking. The raw token is returned to the caller once; only
/// its SHA-256 hash is stored on the shipment.
pub(super) fn generate_tracking_token() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &byte in bytes.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

pub(super) fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Client address for the public tracking rate limit: the first
/// `X-Forwarded-For` entry only when the TCP peer is a configured
/// trusted proxy (`security.trusted_proxies`), otherwise the TCP
/// peer address itself. A direct client can therefore never rotate
/// the header to evade the per-address rate limit. When no peer
/// address is known, "unknown".
///
/// Note: `realip_remote_addr()` is deliberately NOT used as the
/// fallback — Actix trusts `X-Forwarded-For` from any client, so
/// it would return the spoofed header value (see the warning on
/// `unified_core::hardening::client_ip`).
pub(super) fn client_address(req: &HttpRequest, settings: &SecuritySettings) -> String {
    if let Some(ip) = trusted_client_ip_for_http_request(req, settings) {
        return ip.to_string();
    }
    req.peer_addr()
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn load_shipment(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Shipment> {
    let id = parse_uuid(raw_id, "shipment")?;
    state
        .shipments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Shipment not found: {id}")))
}

pub(super) fn load_vehicle(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Vehicle> {
    let id = parse_uuid(raw_id, "vehicle")?;
    state
        .vehicles
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Vehicle not found: {id}")))
}

pub(super) fn load_driver(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Driver> {
    let id = parse_uuid(raw_id, "driver")?;
    state
        .drivers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Driver not found: {id}")))
}

pub(super) fn load_warehouse(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Warehouse> {
    let id = parse_uuid(raw_id, "warehouse")?;
    state
        .warehouses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Warehouse not found: {id}")))
}

pub(super) fn load_route(state: &LogisticsState, raw_id: &str) -> UnifiedResult<DeliveryRoute> {
    let id = parse_uuid(raw_id, "route")?;
    state
        .routes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Delivery route not found: {id}")))
}

pub(super) fn load_route_stop(
    state: &LogisticsState,
    route_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<RouteStop> {
    let id = parse_uuid(raw_id, "route stop")?;
    state
        .route_stops
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|stop| stop.route_id == route_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Route stop not found: {id}")))
}

pub(super) fn load_rate_table(state: &LogisticsState, raw_id: &str) -> UnifiedResult<RateTable> {
    let id = parse_uuid(raw_id, "rate table")?;
    state
        .rate_tables
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Rate table not found: {id}")))
}

pub(super) fn load_exception(
    state: &LogisticsState,
    raw_id: &str,
) -> UnifiedResult<DeliveryException> {
    let id = parse_uuid(raw_id, "exception")?;
    state
        .exceptions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Delivery exception not found: {id}")))
}

/// Ask the store module whether an order id exists, through the
/// `store.order.lookup` hook query. Returns `Some(true)` when the
/// store confirms the order, `Some(false)` when it denies it, and
/// `None` when no handler answers (the store has no answerer yet).
pub(super) fn lookup_order(ctx: &AppContext, order_id: &str) -> Option<bool> {
    let context = ctx
        .hooks
        .emit("store.order.lookup", json!({ "order_id": order_id }));
    for result in &context.results {
        if let Some(found) = result.get("found").and_then(Value::as_bool) {
            return Some(found);
        }
    }
    None
}

/// Ask the Calculator module for a distance in kilometres between
/// two coordinate pairs, through the
/// `calculator.distance_requested` hook query. Returns `Some(km)`
/// when a handler answers with a finite non-negative distance —
/// zero is a legitimate answer (identical origin and destination
/// coordinates), not "no answer" — else `None` (no handler
/// answers today).
pub(super) fn request_distance(
    ctx: &AppContext,
    origin_lat: f64,
    origin_lng: f64,
    dest_lat: f64,
    dest_lng: f64,
) -> Option<f64> {
    let context = ctx.hooks.emit(
        "calculator.distance_requested",
        json!({
            "origin_latitude": origin_lat,
            "origin_longitude": origin_lng,
            "destination_latitude": dest_lat,
            "destination_longitude": dest_lng,
        }),
    );
    for result in &context.results {
        if let Some(km) = result.get("distance_km").and_then(Value::as_f64) {
            if usable_distance_km(km) {
                return Some(km);
            }
        }
    }
    None
}

/// A distance answer is usable when it is finite and non-negative:
/// zero is a legitimate distance (identical origin and destination
/// coordinates), not "no answer". Negative, NaN, and infinite
/// answers are rejected.
pub(super) fn usable_distance_km(km: f64) -> bool {
    km.is_finite() && km >= 0.0
}

/// Dimensional (volumetric) weight in kilograms from centimetre
/// dimensions, using the standard 5000 divisor.
pub(super) fn dimensional_weight(length_cm: f64, width_cm: f64, height_cm: f64) -> f64 {
    (length_cm * width_cm * height_cm) / DIM_DIVISOR
}

/// Straight-line distance in kilometres between two coordinate
/// pairs (haversine). Used only for the nearest-neighbour route
/// reorder heuristic — never as a billable distance.
pub(super) fn haversine_km(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let d_lat = (lat2 - lat1).to_radians();
    let d_lng = (lng2 - lng1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lng / 2.0).sin().powi(2);
    // Clamp before the inverse trig: for near-antipodal points
    // floating-point rounding can push `a` a hair above 1.0, and an
    // unclamped `asin` would return NaN and silently mis-order the
    // route reorder (NaN comparisons are always false). The atan2
    // form is used for the same reason — it stays well-defined at
    // the antipodes.
    let a = a.clamp(0.0, 1.0);
    2.0 * EARTH_RADIUS_KM * a.sqrt().atan2((1.0 - a).sqrt())
}

/// Queue a maintenance reminder through `scheduler.enqueue` with
/// kind `logistics.maintenance.remind`. Records `queued` or
/// `declined` on the reminder (see the scheduler note).
pub(super) fn schedule_maintenance_reminder(
    state: &LogisticsState,
    vehicle_id: Uuid,
    reminder: &mut MaintenanceReminder,
) {
    let Some(bus) = state.bus() else {
        reminder.scheduler_job_id = None;
        reminder.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "logistics.maintenance.remind",
            "payload": {
                "vehicle_id": vehicle_id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "due_date": reminder.due_date.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            reminder.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            reminder.scheduler_status = "queued".to_string();
        }
        _ => {
            reminder.scheduler_job_id = None;
            reminder.scheduler_status = "declined".to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tenant context the tenant-resolution middleware would
    /// attach for a request on a tenant host
    /// (`unified_core::middleware::TenantContext`, the exact struct
    /// `tenancy::TenantContext::from_http_request` reads from the
    /// request extensions). The company is empty, so the resolved
    /// scope is unscoped.
    fn tenant_request() -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            },
        );
        req
    }

        /// A request with a tenant context carrying `tenant_company`
    /// (empty = no tenant company), a verified caller carrying
    /// `verified_company` (empty = no company resolved), and a
    /// forged `X-Company-Id` header when `forged_header` is
    /// non-empty. The header must never affect the scope.
    fn scoped_request(
        tenant_company: &str,
        verified_company: &str,
        forged_header: &str,
    ) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let mut req = actix_web::test::TestRequest::default();
        if !forged_header.is_empty() {
            req = req.insert_header(("x-company-id", forged_header));
        }
        let req = req.to_http_request();
        if !verified_company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(verified_company.to_string()),
            );
        }
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: tenant_company.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = scoped_request("company-a", "company-a", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_uses_verified_caller_when_tenant_has_none() {
        let req = scoped_request("", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-b".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = scoped_request("company-a", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert!(super::caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_a_forged_company_header() {
        let req = scoped_request("", "", "company-forged");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(super::caller_company(&tenant, &req).unwrap(), None);
    }

#[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_matches_uuid_company() {
        let company = uuid::Uuid::now_v7();
        assert!(in_company_scope(&Some(company), &Some(company.to_string())));
        assert!(!in_company_scope(
            &Some(uuid::Uuid::now_v7()),
            &Some(company.to_string())
        ));
        assert!(in_company_scope(&None, &Some(company.to_string())));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(company), &None));
        assert!(!in_company_scope(
            &Some(company),
            &Some("not-a-uuid".to_string())
        ));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).is_err());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn require_text_trims_and_rejects_empty_or_long() {
        assert_eq!(require_text("  hello  ", "name", 10).unwrap(), "hello");
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("123456", "name", 5).is_err());
        assert!(require_text("12345", "name", 5).is_ok());
    }

    #[test]
    fn optional_text_blank_becomes_none() {
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some("   ".to_string()), "note", 10).unwrap(),
            None
        );
        assert_eq!(
            optional_text(&Some(" x ".to_string()), "note", 10).unwrap(),
            Some("x".to_string())
        );
        assert!(optional_text(&Some("toolong".to_string()), "note", 3).is_err());
    }

    #[test]
    fn parse_uuid_valid_and_invalid() {
        let raw = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(
            parse_uuid(raw, "shipment").unwrap(),
            Uuid::parse_str(raw).unwrap()
        );
        assert!(parse_uuid("not-a-uuid", "shipment").is_err());
        assert!(parse_uuid("", "shipment").is_err());
    }

    #[test]
    fn parse_optional_company() {
        assert_eq!(parse_optional_company(&None).unwrap(), None);
        assert_eq!(
            parse_optional_company(&Some("  ".to_string())).unwrap(),
            None
        );
        let raw = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(
            parse_optional_company(&Some(raw.to_string())).unwrap(),
            Some(Uuid::parse_str(raw).unwrap())
        );
        assert!(parse_optional_company(&Some("bad".to_string())).is_err());
    }

    #[test]
    fn valid_money_rounds_to_four_places_and_rejects_bad_values() {
        assert_eq!(valid_money(10.123456, "amount").unwrap(), 10.1235);
        assert_eq!(valid_money(0.0, "amount").unwrap(), 0.0);
        assert!(valid_money(-1.0, "amount").is_err());
        assert!(valid_money(f64::NAN, "amount").is_err());
        assert!(valid_money(f64::INFINITY, "amount").is_err());
    }

    #[test]
    fn round_money_cases() {
        assert_eq!(round_money(1.5), 1.5);
        assert_eq!(round_money(2.00006), 2.0001);
        assert_eq!(round_money(2.00004), 2.0);
        assert_eq!(round_money(0.0), 0.0);
    }

    #[test]
    fn valid_currency_normalizes_and_rejects() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency("  eur ").unwrap(), "EUR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("U1D").is_err());
        assert!(valid_currency("").is_err());
    }

    #[test]
    fn validate_latitude_and_longitude_bounds() {
        assert!(validate_latitude(90.0, "lat").is_ok());
        assert!(validate_latitude(-90.0, "lat").is_ok());
        assert!(validate_latitude(90.0001, "lat").is_err());
        assert!(validate_latitude(f64::NAN, "lat").is_err());
        assert!(validate_longitude(180.0, "lng").is_ok());
        assert!(validate_longitude(-180.0, "lng").is_ok());
        assert!(validate_longitude(180.5, "lng").is_err());
    }

    #[test]
    fn validate_dimensions_rejects_bad_values() {
        assert!(validate_dimensions(10.0, 20.0, 30.0, 40.0).is_ok());
        assert!(validate_dimensions(0.0, 20.0, 30.0, 40.0).is_err());
        assert!(validate_dimensions(10.0, 0.0, 30.0, 40.0).is_err());
        assert!(validate_dimensions(100_000.01, 20.0, 30.0, 40.0).is_err());
        assert!(validate_dimensions(10.0, 10_000.01, 30.0, 40.0).is_err());
    }

    #[test]
    fn validate_window_requires_end_after_start() {
        let start = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let end = DateTime::from_timestamp(1_700_000_600, 0).unwrap();
        assert!(validate_window(Some(start), Some(end)).is_ok());
        assert!(validate_window(Some(end), Some(start)).is_err());
        assert!(validate_window(Some(start), Some(start)).is_err());
        assert!(validate_window(None, Some(end)).is_ok());
        assert!(validate_window(None, None).is_ok());
    }

    #[test]
    fn sha256_hex_known_vector() {
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(sha256_hex("").len(), 64);
    }

    #[test]
    fn valid_sha256_hex_cases() {
        let good = "a".repeat(64);
        assert!(valid_sha256_hex(&good));
        assert!(!valid_sha256_hex(&"a".repeat(63)));
        assert!(!valid_sha256_hex(&"a".repeat(65)));
        assert!(!valid_sha256_hex(&"g".repeat(64)));
        assert!(!valid_sha256_hex(&"A".repeat(64)));
    }

    #[test]
    fn luhn_check_digit_classic_example() {
        assert_eq!(luhn_check_digit("7992739871"), '3');
        assert_eq!(luhn_check_digit(""), '0');
        assert_eq!(luhn_check_digit("0"), '0');
    }

    #[test]
    fn dimensional_weight_uses_5000_divisor() {
        assert_eq!(dimensional_weight(100.0, 100.0, 100.0), 200.0);
        assert_eq!(dimensional_weight(50.0, 40.0, 30.0), 12.0);
    }

    #[test]
    fn haversine_km_same_point_and_one_degree() {
        assert_eq!(haversine_km(0.0, 0.0, 0.0, 0.0), 0.0);
        let one_degree = haversine_km(0.0, 0.0, 0.0, 1.0);
        assert!((one_degree - 111.195).abs() < 0.01, "got {one_degree}");
        assert!(haversine_km(0.0, 0.0, 90.0, 0.0) > 9000.0);
    }

    #[test]
    fn haversine_km_antipodal_points_are_finite_not_nan() {
        // Half the Earth's circumference: ~20,015 km. Before the
        // clamp / atan2 fix this could return NaN, which silently
        // mis-ordered the route reorder (NaN comparisons are false).
        let antipodal = haversine_km(0.0, 0.0, 0.0, 180.0);
        assert!(antipodal.is_finite(), "got {antipodal}");
        assert!((antipodal - 20_015.09).abs() < 1.0, "got {antipodal}");
        let poles = haversine_km(90.0, 0.0, -90.0, 0.0);
        assert!(poles.is_finite(), "got {poles}");
        assert!((poles - 20_015.09).abs() < 1.0, "got {poles}");
        let near_antipodal = haversine_km(0.0, 0.0, 0.0, 179.999_999);
        assert!(near_antipodal.is_finite(), "got {near_antipodal}");
        assert!(near_antipodal > 20_000.0, "got {near_antipodal}");
    }

    #[test]
    fn usable_distance_km_accepts_zero_and_rejects_bad_answers() {
        // Zero is legitimate: identical origin and destination
        // coordinates must quote instead of failing with 422.
        assert!(usable_distance_km(0.0));
        assert!(usable_distance_km(12.5));
        assert!(!usable_distance_km(-0.5));
        assert!(!usable_distance_km(f64::NAN));
        assert!(!usable_distance_km(f64::INFINITY));
        assert!(!usable_distance_km(f64::NEG_INFINITY));
    }

    #[test]
    fn client_address_ignores_x_forwarded_for_from_an_untrusted_peer() {
        use actix_web::test::TestRequest;
        let settings = SecuritySettings::default();
        let req = TestRequest::default()
            .peer_addr("203.0.113.9:1234".parse().unwrap())
            .insert_header(("X-Forwarded-For", "198.51.100.1"))
            .to_http_request();
        assert_eq!(client_address(&req, &settings), "203.0.113.9");
    }

    #[test]
    fn client_address_trusts_x_forwarded_for_from_a_configured_proxy() {
        use actix_web::test::TestRequest;
        let mut settings = SecuritySettings::default();
        settings.trusted_proxies = vec!["203.0.113.9".to_string()];
        let req = TestRequest::default()
            .peer_addr("203.0.113.9:1234".parse().unwrap())
            .insert_header(("X-Forwarded-For", "198.51.100.1, 10.0.0.1"))
            .to_http_request();
        assert_eq!(client_address(&req, &settings), "198.51.100.1");
    }

    #[test]
    fn client_address_without_a_peer_is_unknown() {
        use actix_web::test::TestRequest;
        let settings = SecuritySettings::default();
        let req = TestRequest::default()
            .insert_header(("X-Forwarded-For", "198.51.100.1"))
            .to_http_request();
        assert_eq!(client_address(&req, &settings), "unknown");
    }

    #[test]
    fn generate_tracking_token_is_32_hex_chars() {
        let token = generate_tracking_token();
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_tracking_number_format_and_luhn_property() {
        let state = LogisticsState::default();
        let number = generate_tracking_number(&state).expect("tracking number");
        assert!(number.starts_with("LG-"));
        assert_eq!(number.len(), 13);
        let digits = &number[3..11];
        assert!(digits.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(luhn_check_digit(digits), number.chars().last().unwrap());
    }

    #[test]
    fn paginate_slices_and_reports_totals() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
        let empty = paginate::<i32>(vec![], &PaginationParams::default());
        assert_eq!(empty.pagination.total, 0);
        assert_eq!(empty.pagination.total_pages, 0);
    }
}
