//! HTTP handlers: assets, assignments and depreciation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;
use super::helpers::{minor_to_major};

// Asset handlers

pub(crate) async fn list_assets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("type").cloned();
        let location_filter = params.filters.get("location").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Asset> = state
            .assets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|asset| {
                if let Some(status) = &status_filter {
                    if asset.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(asset_type) = &type_filter {
                    if asset.asset_type.as_str() != asset_type.as_str() {
                        return false;
                    }
                }
                if let Some(location) = &location_filter {
                    if asset.location.as_deref() != Some(location.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        asset.name,
                        asset.description.clone().unwrap_or_default(),
                        asset.serial_number.clone().unwrap_or_default(),
                        asset.vendor.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "purchase_date", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "purchase_date" => a.purchase_date.cmp(&b.purchase_date),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    body: web::Json<CreateAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let asset_type = input.asset_type.unwrap_or_default();
        let serial_number = match &input.serial_number {
            Some(raw) => {
                let serial = require_text(raw, "serial_number", MAX_SERIAL_LEN)?;
                if !serial_is_free(&state, &serial, None) {
                    return Err(UnifiedError::conflict(format!(
                        "Serial number \"{serial}\" is already used by another asset"
                    )));
                }
                Some(serial)
            }
            None => None,
        };
        let purchase_price = match input.purchase_price {
            Some(price) => valid_price(price)?,
            None => 0,
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let vendor = match &input.vendor {
            Some(raw) => Some(require_text(raw, "vendor", MAX_VENDOR_LEN)?),
            None => None,
        };
        let location = match &input.location {
            Some(raw) => Some(require_text(raw, "location", MAX_LOCATION_LEN)?),
            None => None,
        };
        if let Some(license_seats) = input.license_seats {
            if license_seats < 0 {
                return Err(UnifiedError::bad_request("license_seats must be >= 0"));
            }
        }
        if let Some(useful_life) = input.useful_life_years {
            if useful_life <= 0 || useful_life > 100 {
                return Err(UnifiedError::bad_request(
                    "useful_life_years must be between 1 and 100",
                ));
            }
        }
        let salvage_value = match input.salvage_value {
            Some(value) => valid_price(value)?,
            None => 0,
        };
        if salvage_value > purchase_price {
            return Err(UnifiedError::bad_request(
                "salvage_value must not exceed purchase_price",
            ));
        }
        if let Some(warranty_end) = input.warranty_end {
            if let Some(purchase_date) = input.purchase_date {
                if warranty_end < purchase_date {
                    return Err(UnifiedError::bad_request(
                        "warranty_end must not be before purchase_date",
                    ));
                }
            }
        }
        let now = Utc::now();
        let mut asset = Asset {
            id: Uuid::now_v7(),
            name,
            description,
            asset_type,
            serial_number,
            purchase_date: input.purchase_date,
            purchase_price,
            currency,
            vendor,
            location,
            status: AssetStatus::InStock,
            warranty_end: input.warranty_end,
            license_expires_at: input.license_expires_at,
            license_seats: input.license_seats,
            depreciation_method: input.depreciation_method,
            useful_life_years: input.useful_life_years,
            salvage_value,
            depreciation_status: if input.depreciation_method.is_some() {
                "pending".to_string()
            } else {
                "none".to_string()
            },
            depreciation_schedule: None,
            depreciation_error: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        if asset.depreciation_method.is_some() {
            request_depreciation(&ctx, &state, &mut asset);
        }
        state.assets.insert(asset.id, asset.clone());
        state.emit(
            "assets.created",
            json!({
                "asset_id": asset.id.to_string(),
                "name": asset.name,
                "type": asset.asset_type.as_str(),
                "actor": caller.actor,
            }),
        );
        Ok(asset)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        load_asset(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if matches!(asset.status, AssetStatus::Retired | AssetStatus::Disposed) {
            return Err(UnifiedError::conflict(format!(
                "Asset is {} and cannot be edited",
                asset.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.name {
            asset.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.description {
            asset.description = match raw {
                Some(value) => Some(require_text(value, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(asset_type) = input.asset_type {
            asset.asset_type = asset_type;
        }
        if let Some(raw) = &input.serial_number {
            asset.serial_number = match raw {
                Some(value) => {
                    let serial = require_text(value, "serial_number", MAX_SERIAL_LEN)?;
                    if !serial_is_free(&state, &serial, Some(asset.id)) {
                        return Err(UnifiedError::conflict(format!(
                            "Serial number \"{serial}\" is already used by another asset"
                        )));
                    }
                    Some(serial)
                }
                None => None,
            };
        }
        if let Some(raw) = &input.purchase_date {
            asset.purchase_date = *raw;
        }
        if let Some(price) = input.purchase_price {
            asset.purchase_price = valid_price(price)?;
        }
        if let Some(raw) = &input.currency {
            asset.currency = valid_currency(raw)?;
        }
        if let Some(raw) = &input.vendor {
            asset.vendor = match raw {
                Some(value) => Some(require_text(value, "vendor", MAX_VENDOR_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.location {
            asset.location = match raw {
                Some(value) => Some(require_text(value, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.warranty_end {
            asset.warranty_end = *raw;
        }
        if let Some(raw) = &input.license_expires_at {
            asset.license_expires_at = *raw;
        }
        if let Some(raw) = &input.license_seats {
            match raw {
                Some(seats) => {
                    if *seats < 0 {
                        return Err(UnifiedError::bad_request("license_seats must be >= 0"));
                    }
                    asset.license_seats = Some(*seats);
                }
                None => asset.license_seats = None,
            }
        }
        if let Some(raw) = &input.depreciation_method {
            asset.depreciation_method = *raw;
        }
        if let Some(raw) = &input.useful_life_years {
            match raw {
                Some(years) => {
                    if *years <= 0 || *years > 100 {
                        return Err(UnifiedError::bad_request(
                            "useful_life_years must be between 1 and 100",
                        ));
                    }
                    asset.useful_life_years = Some(*years);
                }
                None => asset.useful_life_years = None,
            }
        }
        if let Some(value) = input.salvage_value {
            asset.salvage_value = valid_price(value)?;
        }
        if asset.salvage_value > asset.purchase_price {
            return Err(UnifiedError::bad_request(
                "salvage_value must not exceed purchase_price",
            ));
        }
        if let (Some(warranty_end), Some(purchase_date)) = (asset.warranty_end, asset.purchase_date)
        {
            if warranty_end < purchase_date {
                return Err(UnifiedError::bad_request(
                    "warranty_end must not be before purchase_date",
                ));
            }
        }
        if asset.depreciation_method.is_some() {
            request_depreciation(&ctx, &state, &mut asset);
        } else {
            asset.depreciation_status = "none".to_string();
            asset.depreciation_schedule = None;
            asset.depreciation_error = None;
        }
        asset.updated_at = Utc::now();
        state.assets.insert(asset.id, asset.clone());
        state.emit(
            "assets.updated",
            json!({
                "asset_id": asset.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let asset = load_asset(&state, &path.into_inner())?;
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset has an active assignment and cannot be deleted; return it first",
            ));
        }
        if state
            .assignments
            .iter()
            .any(|entry| entry.value().asset_id == asset.id)
        {
            return Err(UnifiedError::conflict(
                "Asset has assignment history and cannot be deleted; retire it instead",
            ));
        }
        if state
            .stock
            .iter()
            .any(|entry| entry.value().asset_id == asset.id)
        {
            return Err(UnifiedError::conflict(
                "Asset has stock records and cannot be deleted; retire it instead",
            ));
        }
        state.assets.remove(&asset.id);
        state.emit(
            "assets.deleted",
            json!({
                "asset_id": asset.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn assign_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetAssignment> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if asset.status != AssetStatus::InStock {
            return Err(UnifiedError::conflict(format!(
                "Only an in-stock asset can be assigned (asset is {})",
                asset.status.as_str()
            )));
        }
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset already has an active assignment",
            ));
        }
        let user_id = require_text(&body.into_inner().user_id, "user_id", MAX_NAME_LEN)?;
        let now = Utc::now();
        let assignment = AssetAssignment {
            id: Uuid::now_v7(),
            asset_id: asset.id,
            user_id: user_id.clone(),
            assigned_by: caller.actor.clone(),
            assigned_at: now,
            returned_at: None,
            return_reason: None,
            status: "active".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.assignments.insert(assignment.id, assignment.clone());
        asset.status = AssetStatus::Assigned;
        asset.updated_at = now;
        state.assets.insert(asset.id, asset);
        state.emit(
            "assets.assigned",
            json!({
                "asset_id": assignment.asset_id.to_string(),
                "assignment_id": assignment.id.to_string(),
                "user_id": user_id,
                "actor": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn return_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReturnAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetAssignment> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        let Some(mut assignment) = active_assignment(&state, asset.id) else {
            return Err(UnifiedError::conflict(
                "Asset has no active assignment to return",
            ));
        };
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        assignment.returned_at = Some(now);
        assignment.return_reason = reason;
        assignment.status = "returned".to_string();
        assignment.updated_at = now;
        state.assignments.insert(assignment.id, assignment.clone());
        asset.status = AssetStatus::InStock;
        asset.updated_at = now;
        state.assets.insert(asset.id, asset);
        state.emit(
            "assets.returned",
            json!({
                "asset_id": assignment.asset_id.to_string(),
                "assignment_id": assignment.id.to_string(),
                "user_id": assignment.user_id,
                "actor": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared lifecycle transition: retire / dispose / repair moves.
/// Only the transitions listed in the error message are allowed.
fn transition_asset(
    state: &AssetsState,
    asset: &mut Asset,
    from: &[AssetStatus],
    to: AssetStatus,
    event: &str,
    actor: &str,
) -> UnifiedResult<()> {
    if !from.contains(&asset.status) {
        return Err(UnifiedError::conflict(format!(
            "Asset is {} and cannot move to {}",
            asset.status.as_str(),
            to.as_str()
        )));
    }
    asset.status = to;
    asset.updated_at = Utc::now();
    state.assets.insert(asset.id, asset.clone());
    state.emit(
        event,
        json!({
            "asset_id": asset.id.to_string(),
            "status": to.as_str(),
            "actor": actor,
        }),
    );
    Ok(())
}

pub(crate) async fn retire_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset has an active assignment and cannot be retired; return it first",
            ));
        }
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InStock, AssetStatus::InRepair],
            AssetStatus::Retired,
            "assets.retired",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn dispose_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset has an active assignment and cannot be disposed of; return it first",
            ));
        }
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InStock, AssetStatus::InRepair, AssetStatus::Retired],
            AssetStatus::Disposed,
            "assets.disposed",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_for_repair(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if asset.status == AssetStatus::Assigned {
            return Err(UnifiedError::conflict(
                "An assigned asset cannot be sent for repair; return it first",
            ));
        }
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InStock],
            AssetStatus::InRepair,
            "assets.repair.started",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn return_from_repair(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InRepair],
            AssetStatus::InStock,
            "assets.repair.completed",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_assignments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let asset = load_asset(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<AssetAssignment> = state
            .assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|assignment| {
                if assignment.asset_id != asset.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &assignment.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.assigned_at.cmp(&a.assigned_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_depreciation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let asset = load_asset(&state, &path.into_inner())?;
        Ok(json!({
            "asset_id": asset.id,
            "method": asset.depreciation_method.map(|method| method.as_str()),
            "useful_life_years": asset.useful_life_years,
            "purchase_price": minor_to_major(asset.purchase_price, &asset.currency),
            "salvage_value": minor_to_major(asset.salvage_value, &asset.currency),
            "currency": asset.currency,
            "status": asset.depreciation_status,
            "schedule": asset.depreciation_schedule,
            "error": asset.depreciation_error,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn compute_depreciation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if asset.depreciation_method.is_none() {
            return Err(UnifiedError::bad_request(
                "Asset has no depreciation_method configured; set one first",
            ));
        }
        let computed = request_depreciation(&ctx, &state, &mut asset);
        asset.updated_at = Utc::now();
        state.assets.insert(asset.id, asset.clone());
        Ok(json!({
            "asset_id": asset.id,
            "method": asset.depreciation_method.map(|method| method.as_str()),
            "status": asset.depreciation_status,
            "schedule": asset.depreciation_schedule,
            "error": asset.depreciation_error,
            "computed": computed,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_expiring(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let params = query.into_inner();
        let days: i64 = params
            .filters
            .get("days")
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(DEFAULT_EXPIRING_DAYS)
            .clamp(0, MAX_EXPIRING_DAYS);
        let kind_filter = params.filters.get("kind").cloned();
        let today = Utc::now().date_naive();
        let mut items: Vec<Value> = Vec::new();
        for entry in state.assets.iter() {
            let asset = entry.value().clone();
            let mut targets: Vec<(&str, NaiveDate)> = Vec::new();
            if let Some(warranty_end) = asset.warranty_end {
                targets.push(("warranty", warranty_end));
            }
            if asset.asset_type == AssetType::Software {
                if let Some(license_end) = asset.license_expires_at {
                    targets.push(("license", license_end));
                }
            }
            for (kind, expires_at) in targets {
                if let Some(filter) = &kind_filter {
                    if kind != filter.as_str() {
                        continue;
                    }
                }
                let days_until = (expires_at - today).num_days();
                if days_until > days {
                    continue;
                }
                items.push(json!({
                    "asset_id": asset.id,
                    "asset_name": asset.name,
                    "asset_type": asset.asset_type.as_str(),
                    "status": asset.status.as_str(),
                    "kind": kind,
                    "expires_at": expires_at.to_string(),
                    "days_until": days_until,
                }));
            }
        }
        items.sort_by(|a, b| {
            a.get("days_until")
                .and_then(Value::as_i64)
                .cmp(&b.get("days_until").and_then(Value::as_i64))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn run_warranty_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let alerts = run_expiry_check(&state);
        let (queued, job_id) = queue_warranty_check(&state);
        Ok(json!({
            "checked_at": Utc::now().to_rfc3339(),
            "alerts": alerts.len(),
            "alerts_detail": alerts,
            "scheduler_queued": queued,
            "scheduler_job_id": job_id,
            "scheduler_status": if queued { "queued" } else { "declined" },
            "actor": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
