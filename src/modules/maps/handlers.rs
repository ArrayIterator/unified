// Map & Tagging - HTTP handlers.
//
// Auth follows the widgets / saved_views shape: the caller is the
// server-verified JWT identity and every route asks the roles
// module through the `role.check` hook (fail closed). Reads need
// `maps.view`; creates, updates, and deletes need `maps.manage`;
// recording GPS positions needs `maps.track`.
//
// C3 guards (see docs/integration/C3_MODULE_GUARD_MAP.md): every
// handler resolves the tenant FIRST (`require_tenant`) - before
// caller identity or any capability check - then verifies the
// caller's company agrees with the tenant context, and only then
// checks the capability. All reads and writes filter on the
// resolved tenant id, so a tenant can never see or touch another
// tenant's map data. Writes additionally require a resolved
// company scope (`require_caller_company`).

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::tenancy::TenantContext;
use uuid::Uuid;

use super::helpers::{caller, require_caller_company, require_permission, require_tenant, Caller};
use super::store::*;
use super::types::*;
use super::validation::{validate_coordinates, validate_slug};

/// Authorize one request. C3 order: the tenant gate FIRST,
/// then the verified caller, then the capability. Returns the
/// tenant and the caller on success. `require_company` is true
/// for writes: a write needs a resolved company scope.
fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
    require_company: bool,
) -> UnifiedResult<(TenantContext, Caller)> {
    let tenant = require_tenant(req)?;
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    if require_company {
        require_caller_company(&tenant, req)?;
    } else {
        // Reads still verify the company agreement (fail closed
        // on mismatch) without requiring a company to exist.
        super::helpers::caller_company(&tenant, req)?;
    }
    Ok((tenant, found))
}

fn parse_id(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("{what} must be a UUID")))
}

// Locations.

/// `GET /api/v1/maps/locations?page&per_page&kind&layer_id`:
/// this tenant's locations, newest first. Requires `maps.view`.
pub(crate) async fn list_locations_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        let params = query.into_inner();
        let kind = params.filters.get("kind").map(String::as_str);
        if let Some(kind) = kind {
            validate_slug(kind, "kind")?;
        }
        let layer_id = match params.filters.get("layer_id") {
            Some(raw) => Some(parse_id(raw, "layer_id")?),
            None => None,
        };
        list_locations(&ctx, tenant.tenant_id, &params, kind, layer_id).await
    })();
    match result.await {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `POST /api/v1/maps/locations`: create a location.
/// Requires `maps.manage`.
pub(crate) async fn create_location_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateLocationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        create_location(&ctx, tenant.tenant_id, &found.actor, body.into_inner()).await
    })();
    match result.await {
        Ok(location) => json::success_response(location, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `GET /api/v1/maps/locations/{id}`. Requires `maps.view`.
pub(crate) async fn get_location_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        let id = parse_id(&path.into_inner(), "location id")?;
        get_location(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(location) => json::success_response(location, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/maps/locations/{id}`. Requires `maps.manage`.
pub(crate) async fn update_location_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLocationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "location id")?;
        update_location(&ctx, tenant.tenant_id, id, body.into_inner()).await
    })();
    match result.await {
        Ok(location) => json::success_response(location, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/maps/locations/{id}`. Requires `maps.manage`.
pub(crate) async fn delete_location_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "location id")?;
        delete_location(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(location) => json::success_response(location, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Location links.

/// `POST /api/v1/maps/links`: attach a location to a record.
/// Requires `maps.manage`.
pub(crate) async fn attach_link_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<LocationLinkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let input = body.into_inner();
        attach_location(
            &ctx,
            tenant.tenant_id,
            &input.entity_type,
            input.entity_id,
            input.location_id,
            &input.role,
        )
        .await
    })();
    match result.await {
        Ok(link) => json::success_response(link, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/maps/links`: detach a location from a
/// record. Requires `maps.manage`.
pub(crate) async fn detach_link_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<LocationLinkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let input = body.into_inner();
        detach_location(
            &ctx,
            tenant.tenant_id,
            &input.entity_type,
            input.entity_id,
            input.location_id,
            &input.role,
        )
        .await?;
        Ok(serde_json::json!({ "detached": true }))
    })();
    match result.await {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `GET /api/v1/maps/entities/{entity_type}/{entity_id}/locations`:
/// every location attached to one record. Requires `maps.view`.
pub(crate) async fn entity_locations_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        let (entity_type, entity_id) = path.into_inner();
        validate_slug(&entity_type, "entity_type")?;
        let entity_id = parse_id(&entity_id, "entity id")?;
        locations_for_entity(&ctx, tenant.tenant_id, &entity_type, entity_id).await
    })();
    match result.await {
        Ok(locations) => json::success_response(locations, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Nearby search.

/// `GET /api/v1/maps/nearby?latitude&longitude&radius_km&kind`.
#[derive(Debug, Deserialize)]
pub(crate) struct NearbyQuery {
    pub latitude: f64,
    pub longitude: f64,
    pub radius_km: Option<f64>,
    pub kind: Option<String>,
}

/// `GET /api/v1/maps/nearby`: locations within `radius_km`
/// (default 10) of the point, nearest first. Requires
/// `maps.view`.
pub(crate) async fn nearby_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<NearbyQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        let query = query.into_inner();
        validate_coordinates(query.latitude, query.longitude)?;
        let radius_km = query.radius_km.unwrap_or(10.0);
        nearby(
            &ctx,
            tenant.tenant_id,
            query.latitude,
            query.longitude,
            radius_km,
            query.kind.as_deref(),
        )
        .await
    })();
    match result.await {
        Ok(locations) => json::success_response(locations, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Layers.

/// `GET /api/v1/maps/layers`. Requires `maps.view`.
pub(crate) async fn list_layers_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        list_layers(&ctx, tenant.tenant_id).await
    })();
    match result.await {
        Ok(layers) => json::success_response(layers, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `POST /api/v1/maps/layers`. Requires `maps.manage`.
pub(crate) async fn create_layer_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateLayerInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        create_layer(&ctx, tenant.tenant_id, &found.actor, body.into_inner()).await
    })();
    match result.await {
        Ok(layer) => json::success_response(layer, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/maps/layers/{id}`. Requires `maps.manage`.
pub(crate) async fn update_layer_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLayerInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "layer id")?;
        update_layer(&ctx, tenant.tenant_id, id, body.into_inner()).await
    })();
    match result.await {
        Ok(layer) => json::success_response(layer, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/maps/layers/{id}`. Requires `maps.manage`.
pub(crate) async fn delete_layer_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "layer id")?;
        delete_layer(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(layer) => json::success_response(layer, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Geofences.

/// `GET /api/v1/maps/geofences`. Requires `maps.view`.
pub(crate) async fn list_geofences_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        list_geofences(&ctx, tenant.tenant_id).await
    })();
    match result.await {
        Ok(geofences) => json::success_response(geofences, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `POST /api/v1/maps/geofences`. Requires `maps.manage`.
pub(crate) async fn create_geofence_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateGeofenceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        create_geofence(&ctx, tenant.tenant_id, &found.actor, body.into_inner()).await
    })();
    match result.await {
        Ok(geofence) => json::success_response(geofence, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `GET /api/v1/maps/geofences/{id}`. Requires `maps.view`.
pub(crate) async fn get_geofence_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        let id = parse_id(&path.into_inner(), "geofence id")?;
        get_geofence(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(geofence) => json::success_response(geofence, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/maps/geofences/{id}`. Requires `maps.manage`.
pub(crate) async fn update_geofence_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateGeofenceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "geofence id")?;
        update_geofence(&ctx, tenant.tenant_id, id, body.into_inner()).await
    })();
    match result.await {
        Ok(geofence) => json::success_response(geofence, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/maps/geofences/{id}`. Requires `maps.manage`.
pub(crate) async fn delete_geofence_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "geofence id")?;
        delete_geofence(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(geofence) => json::success_response(geofence, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `POST /api/v1/maps/geofences/{id}/contains`: test one point
/// against one zone. Requires `maps.view`.
pub(crate) async fn contains_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ContainsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        let id = parse_id(&path.into_inner(), "geofence id")?;
        let input = body.into_inner();
        validate_coordinates(input.latitude, input.longitude)?;
        let contains =
            within_geofence(&ctx, tenant.tenant_id, id, input.latitude, input.longitude).await?;
        let fence = get_geofence(&ctx, tenant.tenant_id, id).await?;
        Ok(ContainsResult {
            geofence_id: id,
            name: fence.name,
            contains,
        })
    })();
    match result.await {
        Ok(result) => json::success_response(result, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Positions.

/// `POST /api/v1/maps/positions`: record one GPS position.
/// Requires `maps.track`.
pub(crate) async fn record_position_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RecordPositionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_TRACK, true)?;
        record_position(&ctx, tenant.tenant_id, body.into_inner()).await
    })();
    match result.await {
        Ok(position) => json::success_response(position, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `GET /api/v1/maps/positions/{entity_type}/{entity_id}?from&to`:
/// position history for one entity, newest first. Requires
/// `maps.view`.
#[derive(Debug, Deserialize)]
pub(crate) struct PositionsQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

pub(crate) async fn list_positions_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<PositionsQuery>,
    page: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        let (entity_type, entity_id) = path.into_inner();
        validate_slug(&entity_type, "entity_type")?;
        let entity_id = parse_id(&entity_id, "entity id")?;
        let query = query.into_inner();
        let params = page.into_inner();
        list_positions(
            &ctx,
            tenant.tenant_id,
            &params,
            &entity_type,
            entity_id,
            query.from,
            query.to,
        )
        .await
    })();
    match result.await {
        Ok(positions) => json::success_response(positions, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Kinds and roles (admin-definable definitions).

/// `GET /api/v1/maps/kinds`. Requires `maps.view`.
pub(crate) async fn list_kinds_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        list_kinds(&ctx, tenant.tenant_id).await
    })();
    match result.await {
        Ok(kinds) => json::success_response(kinds, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `POST /api/v1/maps/kinds`. Requires `maps.manage`.
pub(crate) async fn create_kind_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateKindInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        create_kind(&ctx, tenant.tenant_id, body.into_inner()).await
    })();
    match result.await {
        Ok(kind) => json::success_response(kind, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/maps/kinds/{id}`. Requires `maps.manage`.
pub(crate) async fn update_kind_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateKindInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "kind id")?;
        update_kind(&ctx, tenant.tenant_id, id, body.into_inner()).await
    })();
    match result.await {
        Ok(kind) => json::success_response(kind, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/maps/kinds/{id}`. Requires `maps.manage`.
pub(crate) async fn delete_kind_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "kind id")?;
        delete_kind(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(kind) => json::success_response(kind, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `GET /api/v1/maps/roles`. Requires `maps.view`.
pub(crate) async fn list_roles_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        list_roles(&ctx, tenant.tenant_id).await
    })();
    match result.await {
        Ok(roles) => json::success_response(roles, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `POST /api/v1/maps/roles`. Requires `maps.manage`.
pub(crate) async fn create_role_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        create_role(&ctx, tenant.tenant_id, body.into_inner()).await
    })();
    match result.await {
        Ok(role) => json::success_response(role, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/maps/roles/{id}`. Requires `maps.manage`.
pub(crate) async fn update_role_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "role id")?;
        update_role(&ctx, tenant.tenant_id, id, body.into_inner()).await
    })();
    match result.await {
        Ok(role) => json::success_response(role, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/maps/roles/{id}`. Requires `maps.manage`.
pub(crate) async fn delete_role_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "role id")?;
        delete_role(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(role) => json::success_response(role, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Providers and settings.

/// `GET /api/v1/maps/providers`. Requires `maps.view`.
pub(crate) async fn list_providers_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        list_providers(&ctx, tenant.tenant_id).await
    })();
    match result.await {
        Ok(providers) => json::success_response(providers, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `POST /api/v1/maps/providers`. Requires `maps.manage`.
pub(crate) async fn create_provider_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateProviderInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        create_provider(&ctx, tenant.tenant_id, body.into_inner()).await
    })();
    match result.await {
        Ok(provider) => json::success_response(provider, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/maps/providers/{id}`. Requires `maps.manage`.
pub(crate) async fn update_provider_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProviderInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "provider id")?;
        update_provider(&ctx, tenant.tenant_id, id, body.into_inner()).await
    })();
    match result.await {
        Ok(provider) => json::success_response(provider, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/maps/providers/{id}`. Requires `maps.manage`.
pub(crate) async fn delete_provider_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        let id = parse_id(&path.into_inner(), "provider id")?;
        delete_provider(&ctx, tenant.tenant_id, id).await
    })();
    match result.await {
        Ok(provider) => json::success_response(provider, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `GET /api/v1/maps/settings`: the resolved map
/// configuration (tile URL with the key substituted when one
/// is stored). The key itself is never returned. Requires
/// `maps.view`.
pub(crate) async fn get_settings_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_VIEW, false)?;
        resolve_settings(&ctx, tenant.tenant_id).await
    })();
    match result.await {
        Ok(settings) => json::success_response(settings, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/maps/settings`: save the tenant's active
/// provider, default centre/zoom, and (write-only) browser API
/// key. Requires `maps.manage`.
pub(crate) async fn save_settings_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<SaveSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let (tenant, _found) = authorize(&ctx, &req, CAP_MAPS_MANAGE, true)?;
        save_settings(&ctx, tenant.tenant_id, body.into_inner()).await
    })();
    match result.await {
        Ok(settings) => json::success_response(settings, 200),
        Err(error) => json::error_response(&error, debug),
    }
}
