// Map & Tagging - Diesel-backed store and the public contract
// other modules call.
//
// The database is the ONLY authority for stored map data. Reads
// without a configured pool fail with 503 (a map read must never
// silently return another tenant's data, and there is no safe
// default to degrade to); writes without a pool fail the same
// way. There is no in-memory cache: every read hits the table,
// so a location can never go stale in this process. Blocking
// Diesel work runs off the async worker threads, like the
// widgets module.
//
// Tenant scoping: EVERY query filters on the resolved tenant
// id, and every single-record load is followed by a tenant
// check in the caller. A tenant can never see or touch another
// tenant's locations, links, layers, geofences, positions,
// kinds, roles, providers, or settings.
//
// Public contract (the stable surface other modules consume):
//   `attach_location` / `detach_location`
//   `locations_for_entity`
//   `nearby`
//   `within_geofence` / `geofences_containing`
//   `record_position`
// Each takes the resolved tenant id, so a consumer can never
// forget the scope: there is no unscoped entry point.

use chrono::{DateTime, Utc};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::db::schema::{
    map_geofences, map_layers, map_link_roles, map_location_kinds, map_location_links,
    map_locations, map_positions, map_providers, map_settings,
};
use unified_core::db::DbPool;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

use super::crypto::{decrypt_optional, encrypt_secret};
use super::providers::{
    parse_subdomains, resolve_tile_url, validate_max_zoom, validate_subdomains,
};
use super::types::*;
use super::validation::{
    bounding_box, haversine_km, point_in_geometry, validate_color, validate_coordinates,
    validate_entity_type, validate_geometry, validate_json_object, validate_optional_text,
    validate_slug, validate_text, validate_tile_template,
};

// Database plumbing.

/// The database pool, or 503 when no database is configured.
pub(crate) fn db_pool(ctx: &AppContext) -> UnifiedResult<DbPool> {
    ctx.db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new(
                "Database is not configured: Map & Tagging needs a database connection",
                503,
            )
            .with_metadata("kind", json!("database_not_configured"))
        })
}

/// Run blocking Diesel work off the async worker threads.
pub(crate) async fn run_blocking<F, T>(pool: DbPool, work: F) -> UnifiedResult<T>
where
    F: FnOnce(&mut PgConnection) -> UnifiedResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        work(&mut connection)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Database task failed: {error}")))?
}

// Rows (Diesel) and their public shapes.

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_locations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct LocationRow {
    id: Uuid,
    tenant_id: Uuid,
    layer_id: Option<Uuid>,
    label: String,
    kind: String,
    latitude: f64,
    longitude: f64,
    address: Option<String>,
    metadata: Value,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<LocationRow> for MapLocation {
    fn from(row: LocationRow) -> Self {
        Self {
            id: row.id,
            tenant_id: row.tenant_id,
            layer_id: row.layer_id,
            label: row.label,
            kind: row.kind,
            latitude: row.latitude,
            longitude: row.longitude,
            address: row.address,
            metadata: row.metadata,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_location_links)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct LinkRow {
    tenant_id: Uuid,
    entity_type: String,
    entity_id: Uuid,
    location_id: Uuid,
    role: String,
    created_at: DateTime<Utc>,
}

impl From<LinkRow> for LocationLink {
    fn from(row: LinkRow) -> Self {
        Self {
            tenant_id: row.tenant_id,
            entity_type: row.entity_type,
            entity_id: row.entity_id,
            location_id: row.location_id,
            role: row.role,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_layers)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct LayerRow {
    id: Uuid,
    tenant_id: Uuid,
    name: String,
    description: Option<String>,
    visible: bool,
    style: Value,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<LayerRow> for MapLayer {
    fn from(row: LayerRow) -> Self {
        Self {
            id: row.id,
            tenant_id: row.tenant_id,
            name: row.name,
            description: row.description,
            visible: row.visible,
            style: row.style,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_geofences)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct GeofenceRow {
    id: Uuid,
    tenant_id: Uuid,
    name: String,
    description: Option<String>,
    geometry: Value,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<GeofenceRow> for MapGeofence {
    fn from(row: GeofenceRow) -> Self {
        Self {
            id: row.id,
            tenant_id: row.tenant_id,
            name: row.name,
            description: row.description,
            geometry: row.geometry,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_positions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct PositionRow {
    id: Uuid,
    tenant_id: Uuid,
    entity_type: String,
    entity_id: Uuid,
    latitude: f64,
    longitude: f64,
    accuracy_m: Option<f64>,
    speed_kmh: Option<f64>,
    heading_deg: Option<f64>,
    source: String,
    recorded_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

impl From<PositionRow> for MapPosition {
    fn from(row: PositionRow) -> Self {
        Self {
            id: row.id,
            tenant_id: row.tenant_id,
            entity_type: row.entity_type,
            entity_id: row.entity_id,
            latitude: row.latitude,
            longitude: row.longitude,
            accuracy_m: row.accuracy_m,
            speed_kmh: row.speed_kmh,
            heading_deg: row.heading_deg,
            source: row.source,
            recorded_at: row.recorded_at,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_location_kinds)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct KindRow {
    id: Uuid,
    tenant_id: Uuid,
    slug: String,
    label: String,
    icon: String,
    color: String,
    enabled: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<KindRow> for LocationKind {
    fn from(row: KindRow) -> Self {
        Self {
            id: Some(row.id),
            tenant_id: Some(row.tenant_id),
            slug: row.slug,
            label: row.label,
            icon: row.icon,
            color: row.color,
            enabled: row.enabled,
            is_builtin: false,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_link_roles)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct RoleRow {
    id: Uuid,
    tenant_id: Uuid,
    slug: String,
    label: String,
    enabled: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<RoleRow> for LinkRole {
    fn from(row: RoleRow) -> Self {
        Self {
            id: Some(row.id),
            tenant_id: Some(row.tenant_id),
            slug: row.slug,
            label: row.label,
            enabled: row.enabled,
            is_builtin: false,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_providers)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct ProviderRow {
    id: Uuid,
    tenant_id: Option<Uuid>,
    slug: String,
    display_name: String,
    tile_url_template: String,
    attribution: String,
    requires_api_key: bool,
    max_zoom: i32,
    subdomains: String,
    options: Value,
    enabled: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<ProviderRow> for MapProvider {
    fn from(row: ProviderRow) -> Self {
        Self {
            id: row.id,
            tenant_id: row.tenant_id,
            slug: row.slug,
            display_name: row.display_name,
            tile_url_template: row.tile_url_template,
            attribution: row.attribution,
            requires_api_key: row.requires_api_key,
            max_zoom: row.max_zoom,
            subdomains: row.subdomains,
            options: row.options,
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = map_settings)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct SettingsRow {
    tenant_id: Uuid,
    active_provider: String,
    api_key_ciphertext: Option<String>,
    default_latitude: f64,
    default_longitude: f64,
    default_zoom: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Load one row by id, scoped to the tenant: a row of another
/// tenant is "not found", never a cross-tenant read.
fn load_location_row(
    connection: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<LocationRow> {
    map_locations::table
        .filter(map_locations::tenant_id.eq(tenant_id))
        .filter(map_locations::id.eq(id))
        .select(LocationRow::as_select())
        .first::<LocationRow>(connection)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load map location: {error}")))?
        .ok_or_else(|| UnifiedError::not_found("Map location not found"))
}

fn load_layer_row(
    connection: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<LayerRow> {
    map_layers::table
        .filter(map_layers::tenant_id.eq(tenant_id))
        .filter(map_layers::id.eq(id))
        .select(LayerRow::as_select())
        .first::<LayerRow>(connection)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load map layer: {error}")))?
        .ok_or_else(|| UnifiedError::not_found("Map layer not found"))
}

fn load_geofence_row(
    connection: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<GeofenceRow> {
    map_geofences::table
        .filter(map_geofences::tenant_id.eq(tenant_id))
        .filter(map_geofences::id.eq(id))
        .select(GeofenceRow::as_select())
        .first::<GeofenceRow>(connection)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load map geofence: {error}")))?
        .ok_or_else(|| UnifiedError::not_found("Map geofence not found"))
}

fn load_kind_row(
    connection: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<KindRow> {
    map_location_kinds::table
        .filter(map_location_kinds::tenant_id.eq(tenant_id))
        .filter(map_location_kinds::id.eq(id))
        .select(KindRow::as_select())
        .first::<KindRow>(connection)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load location kind: {error}")))?
        .ok_or_else(|| UnifiedError::not_found("Location kind not found"))
}

fn load_role_row(
    connection: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<RoleRow> {
    map_link_roles::table
        .filter(map_link_roles::tenant_id.eq(tenant_id))
        .filter(map_link_roles::id.eq(id))
        .select(RoleRow::as_select())
        .first::<RoleRow>(connection)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load link role: {error}")))?
        .ok_or_else(|| UnifiedError::not_found("Link role not found"))
}

fn load_provider_row(
    connection: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<ProviderRow> {
    // A provider is visible when it is a platform definition
    // (tenant_id NULL) or the caller's own tenant row. Two plain
    // queries instead of one OR filter: the tenant row wins
    // when both exist (a tenant override shadows the platform
    // definition with the same slug).
    let tenant_row = map_providers::table
        .filter(map_providers::tenant_id.eq(tenant_id))
        .filter(map_providers::id.eq(id))
        .select(ProviderRow::as_select())
        .first::<ProviderRow>(connection)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load map provider: {error}")))?;
    if let Some(row) = tenant_row {
        return Ok(row);
    }
    map_providers::table
        .filter(map_providers::tenant_id.is_null())
        .filter(map_providers::id.eq(id))
        .select(ProviderRow::as_select())
        .first::<ProviderRow>(connection)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load map provider: {error}")))?
        .ok_or_else(|| UnifiedError::not_found("Map provider not found"))
}

/// A location kind is usable when it is a built-in or a
/// tenant-defined, enabled kind. Unknown or disabled kinds are
/// 400, never silently accepted (kinds are definitions, not
/// free text).
async fn kind_is_usable(ctx: &Arc<AppContext>, tenant_id: Uuid, slug: &str) -> UnifiedResult<bool> {
    let pool = db_pool(ctx)?;
    let tenant = tenant_id;
    let wanted = slug.to_string();
    let stored = run_blocking(pool, move |connection| {
        map_location_kinds::table
            .filter(map_location_kinds::tenant_id.eq(tenant))
            .filter(map_location_kinds::slug.eq(&wanted))
            .select(KindRow::as_select())
            .first::<KindRow>(connection)
            .optional()
            .map_err(|error| UnifiedError::internal(format!("Cannot load location kind: {error}")))
    })
    .await?;
    Ok(match stored {
        Some(row) => row.enabled,
        None => builtin_location_kinds()
            .iter()
            .any(|kind| kind.slug == slug && kind.enabled),
    })
}

/// A link role is usable under the same rule as a kind.
async fn role_is_usable(ctx: &Arc<AppContext>, tenant_id: Uuid, slug: &str) -> UnifiedResult<bool> {
    let pool = db_pool(ctx)?;
    let tenant = tenant_id;
    let wanted = slug.to_string();
    let stored = run_blocking(pool, move |connection| {
        map_link_roles::table
            .filter(map_link_roles::tenant_id.eq(tenant))
            .filter(map_link_roles::slug.eq(&wanted))
            .select(RoleRow::as_select())
            .first::<RoleRow>(connection)
            .optional()
            .map_err(|error| UnifiedError::internal(format!("Cannot load link role: {error}")))
    })
    .await?;
    Ok(match stored {
        Some(row) => row.enabled,
        None => builtin_link_roles()
            .iter()
            .any(|role| role.slug == slug && role.enabled),
    })
}

// Locations.

/// Map a Diesel error to an API error: a unique-violation is a
/// 409 naming the conflicting value; anything else is a 500.
/// The database message is never rendered (it can contain a
/// value the caller must not see echoed back raw).
fn map_db_error(error: diesel::result::Error, what: &str) -> UnifiedError {
    match error {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => UnifiedError::conflict(format!("{what} already exists")),
        other => UnifiedError::internal(format!("Cannot save {what}: {other}")),
    }
}

/// Create a location: validates every field, checks the kind
/// is a known definition and the layer (when given) belongs to
/// this tenant, then inserts and emits `maps.location_created`.
pub(crate) async fn create_location(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    actor: &str,
    input: CreateLocationInput,
) -> UnifiedResult<MapLocation> {
    let label = validate_text(&input.label, "label", 200)?;
    validate_slug(&input.kind, "kind")?;
    validate_coordinates(input.latitude, input.longitude)?;
    let address = validate_optional_text(&input.address, "address", 1000)?;
    validate_json_object(&input.metadata, "metadata")?;
    if !kind_is_usable(ctx, tenant_id, &input.kind).await? {
        return Err(UnifiedError::bad_request(format!(
            "Unknown or disabled location kind \"{}\"",
            input.kind
        )));
    }
    let pool = db_pool(ctx)?;
    let layer_id = input.layer_id;
    if let Some(layer) = layer_id {
        run_blocking(pool.clone(), move |connection| {
            load_layer_row(connection, tenant_id, layer).map(|_| ())
        })
        .await?;
    }
    let row = LocationRow {
        id: Uuid::now_v7(),
        tenant_id,
        layer_id,
        label,
        kind: input.kind,
        latitude: input.latitude,
        longitude: input.longitude,
        address,
        metadata: input.metadata,
        created_by: actor.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_locations::table)
            .values(&stored)
            .execute(connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot save map location: {error}"))
            })?;
        Ok(())
    })
    .await?;
    let location = MapLocation::from(row);
    ctx.hooks.emit(
        events::MAPS_LOCATION_CREATED,
        json!({
            "tenant_id": tenant_id.to_string(),
            "location_id": location.id.to_string(),
            "kind": location.kind,
            "latitude": location.latitude,
            "longitude": location.longitude,
        }),
    );
    Ok(location)
}

/// Load one location of this tenant (404 for another tenant's
/// row, never a cross-tenant read).
pub(crate) async fn get_location(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<MapLocation> {
    let pool = db_pool(ctx)?;
    let row = run_blocking(pool, move |connection| {
        load_location_row(connection, tenant_id, id)
    })
    .await?;
    Ok(MapLocation::from(row))
}

/// List this tenant's locations, newest first. Filters: `kind`
/// and `layer_id` (exact match, from the query's extra
/// filters). Paginated with the shared `page` / `per_page`.
pub(crate) async fn list_locations(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    params: &PaginationParams,
    kind: Option<&str>,
    layer_id: Option<Uuid>,
) -> UnifiedResult<PageResult<MapLocation>> {
    let pagination = params.normalized();
    let pool = db_pool(ctx)?;
    let offset = params.offset();
    let limit = params.limit();
    let kind_filter = kind.map(str::to_string);
    let total = run_blocking(pool.clone(), move |connection| {
        let mut query = map_locations::table
            .filter(map_locations::tenant_id.eq(tenant_id))
            .into_boxed();
        if let Some(kind) = &kind_filter {
            query = query.filter(map_locations::kind.eq(kind));
        }
        if let Some(layer) = layer_id {
            query = query.filter(map_locations::layer_id.eq(layer));
        }
        query
            .count()
            .get_result::<i64>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot count map locations: {error}")))
    })
    .await?;
    let kind_filter = kind.map(str::to_string);
    let rows = run_blocking(pool, move |connection| {
        let mut query = map_locations::table
            .filter(map_locations::tenant_id.eq(tenant_id))
            .into_boxed();
        if let Some(kind) = &kind_filter {
            query = query.filter(map_locations::kind.eq(kind));
        }
        if let Some(layer) = layer_id {
            query = query.filter(map_locations::layer_id.eq(layer));
        }
        query
            .order((map_locations::created_at.desc(), map_locations::id.desc()))
            .offset(offset)
            .limit(limit)
            .select(LocationRow::as_select())
            .load::<LocationRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list map locations: {error}")))
    })
    .await?;
    Ok(PageResult::new(
        rows.into_iter().map(MapLocation::from).collect(),
        total as u64,
        &pagination,
    ))
}

/// Update a location: omitted fields keep their stored value;
/// `address: null` / `layer_id: null` clear the field. The
/// kind (when changed) must still be a known definition, and a
/// new layer must belong to this tenant.
pub(crate) async fn update_location(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
    input: UpdateLocationInput,
) -> UnifiedResult<MapLocation> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_location_row(connection, tenant_id, id)
    })
    .await?;
    let label = match &input.label {
        Some(value) => validate_text(value, "label", 200)?,
        None => current.label.clone(),
    };
    let kind = match &input.kind {
        Some(value) => {
            validate_slug(value, "kind")?;
            value.clone()
        }
        None => current.kind.clone(),
    };
    let latitude = input.latitude.unwrap_or(current.latitude);
    let longitude = input.longitude.unwrap_or(current.longitude);
    validate_coordinates(latitude, longitude)?;
    let address = match &input.address {
        Some(value) => validate_optional_text(value, "address", 1000)?,
        None => current.address.clone(),
    };
    let layer_id = match &input.layer_id {
        Some(value) => *value,
        None => current.layer_id,
    };
    let metadata = match &input.metadata {
        Some(value) => {
            validate_json_object(value, "metadata")?;
            value.clone()
        }
        None => current.metadata.clone(),
    };
    if kind != current.kind && !kind_is_usable(ctx, tenant_id, &kind).await? {
        return Err(UnifiedError::bad_request(format!(
            "Unknown or disabled location kind \"{kind}\""
        )));
    }
    if layer_id != current.layer_id {
        if let Some(layer) = layer_id {
            run_blocking(pool.clone(), move |connection| {
                load_layer_row(connection, tenant_id, layer).map(|_| ())
            })
            .await?;
        }
    }
    let now = Utc::now();
    let updated = run_blocking(pool, move |connection| {
        diesel::update(
            map_locations::table
                .filter(map_locations::tenant_id.eq(tenant_id))
                .filter(map_locations::id.eq(id)),
        )
        .set((
            map_locations::label.eq(&label),
            map_locations::kind.eq(&kind),
            map_locations::latitude.eq(latitude),
            map_locations::longitude.eq(longitude),
            map_locations::address.eq(&address),
            map_locations::layer_id.eq(layer_id),
            map_locations::metadata.eq(&metadata),
            map_locations::updated_at.eq(now),
        ))
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot update map location: {error}")))?;
        load_location_row(connection, tenant_id, id)
    })
    .await?;
    let location = MapLocation::from(updated);
    ctx.hooks.emit(
        events::MAPS_LOCATION_UPDATED,
        json!({
            "tenant_id": tenant_id.to_string(),
            "location_id": location.id.to_string(),
            "kind": location.kind,
        }),
    );
    Ok(location)
}

/// Delete a location. Its links disappear with it (the
/// database cascade); the owning records are untouched.
pub(crate) async fn delete_location(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<MapLocation> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_location_row(connection, tenant_id, id)
    })
    .await?;
    run_blocking(pool, move |connection| {
        diesel::delete(
            map_locations::table
                .filter(map_locations::tenant_id.eq(tenant_id))
                .filter(map_locations::id.eq(id)),
        )
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot delete map location: {error}")))?;
        Ok(())
    })
    .await?;
    ctx.hooks.emit(
        events::MAPS_LOCATION_DELETED,
        json!({
            "tenant_id": tenant_id.to_string(),
            "location_id": current.id.to_string(),
        }),
    );
    Ok(MapLocation::from(current))
}

/// Nearby search (public contract): locations within
/// `radius_km` of the point, nearest first. A bounding-box
/// prefilter runs in SQL; the exact haversine distance is
/// computed per row and rows outside the radius are dropped.
pub async fn nearby(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    latitude: f64,
    longitude: f64,
    radius_km: f64,
    kind: Option<&str>,
) -> UnifiedResult<Vec<NearbyLocation>> {
    validate_coordinates(latitude, longitude)?;
    if !radius_km.is_finite() || radius_km <= 0.0 || radius_km > MAX_NEARBY_RADIUS_KM {
        return Err(UnifiedError::bad_request(format!(
            "radius_km must be between 0 and {MAX_NEARBY_RADIUS_KM}"
        )));
    }
    if let Some(kind) = kind {
        validate_slug(kind, "kind")?;
    }
    let (min_lat, max_lat, min_lng, max_lng) = bounding_box(latitude, longitude, radius_km);
    let pool = db_pool(ctx)?;
    let kind_filter = kind.map(str::to_string);
    let rows = run_blocking(pool, move |connection| {
        let mut query = map_locations::table
            .filter(map_locations::tenant_id.eq(tenant_id))
            .filter(map_locations::latitude.ge(min_lat))
            .filter(map_locations::latitude.le(max_lat))
            .filter(map_locations::longitude.ge(min_lng))
            .filter(map_locations::longitude.le(max_lng))
            .into_boxed();
        if let Some(kind) = &kind_filter {
            query = query.filter(map_locations::kind.eq(kind));
        }
        query
            .order((map_locations::created_at.desc(), map_locations::id.desc()))
            .limit(MAX_NEARBY_RESULTS as i64)
            .select(LocationRow::as_select())
            .load::<LocationRow>(connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot search nearby locations: {error}"))
            })
    })
    .await?;
    let mut found: Vec<NearbyLocation> = rows
        .into_iter()
        .map(MapLocation::from)
        .map(|location| {
            let distance_km =
                haversine_km(latitude, longitude, location.latitude, location.longitude);
            NearbyLocation {
                location,
                distance_km,
            }
        })
        .filter(|nearby| nearby.distance_km <= radius_km)
        .collect();
    found.sort_by(|a, b| {
        a.distance_km
            .partial_cmp(&b.distance_km)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(found)
}

// Location links - the generic join and the public contract.

/// Attach a location to another module's record (public
/// contract). The location must belong to this tenant and the
/// role must be a known definition. Attaching the same
/// (entity, location, role) twice is a no-op returning the
/// existing link. Emits `maps.location_attached` so the owning
/// module (or any listener) can react.
pub async fn attach_location(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    location_id: Uuid,
    role: &str,
) -> UnifiedResult<LocationLink> {
    validate_entity_type(entity_type)?;
    validate_slug(role, "role")?;
    if !role_is_usable(ctx, tenant_id, role).await? {
        return Err(UnifiedError::bad_request(format!(
            "Unknown or disabled link role \"{role}\""
        )));
    }
    let pool = db_pool(ctx)?;
    // Own the borrowed strings: the blocking closure below
    // must be 'static (spawn_blocking), so it cannot borrow
    // the caller's parameters.
    let entity_type = entity_type.to_string();
    let role = role.to_string();
    run_blocking(pool.clone(), move |connection| {
        load_location_row(connection, tenant_id, location_id).map(|_| ())
    })
    .await?;
    let row = LinkRow {
        tenant_id,
        entity_type: entity_type.clone(),
        entity_id,
        location_id,
        role: role.clone(),
        created_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool.clone(), move |connection| {
        diesel::insert_into(map_location_links::table)
            .values(&stored)
            .on_conflict((
                map_location_links::tenant_id,
                map_location_links::entity_type,
                map_location_links::entity_id,
                map_location_links::location_id,
                map_location_links::role,
            ))
            .do_nothing()
            .execute(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot attach location: {error}")))?;
        Ok(())
    })
    .await?;
    let link = run_blocking(pool, move |connection| {
        map_location_links::table
            .filter(map_location_links::tenant_id.eq(tenant_id))
            .filter(map_location_links::entity_type.eq(entity_type))
            .filter(map_location_links::entity_id.eq(entity_id))
            .filter(map_location_links::location_id.eq(location_id))
            .filter(map_location_links::role.eq(role))
            .select(LinkRow::as_select())
            .first::<LinkRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot load location link: {error}")))
    })
    .await?;
    let link = LocationLink::from(link);
    ctx.hooks.emit(
        events::MAPS_LOCATION_ATTACHED,
        json!({
            "tenant_id": tenant_id.to_string(),
            "entity_type": link.entity_type,
            "entity_id": link.entity_id.to_string(),
            "location_id": link.location_id.to_string(),
            "role": link.role,
        }),
    );
    Ok(link)
}

/// Detach a location from a record (public contract). A link
/// that does not exist is 404. Emits `maps.location_detached`.
pub async fn detach_location(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    location_id: Uuid,
    role: &str,
) -> UnifiedResult<()> {
    validate_entity_type(entity_type)?;
    validate_slug(role, "role")?;
    let pool = db_pool(ctx)?;
    // Own the borrowed strings: the blocking closure below
    // must be 'static (spawn_blocking), so it cannot borrow
    // the caller's parameters.
    let entity_type = entity_type.to_string();
    let role = role.to_string();
    // Separate copies for the blocking closure: the originals
    // are still needed for the hook payload below.
    let entity_type_for_delete = entity_type.clone();
    let role_for_delete = role.clone();
    let deleted = run_blocking(pool, move |connection| {
        diesel::delete(
            map_location_links::table
                .filter(map_location_links::tenant_id.eq(tenant_id))
                .filter(map_location_links::entity_type.eq(entity_type_for_delete))
                .filter(map_location_links::entity_id.eq(entity_id))
                .filter(map_location_links::location_id.eq(location_id))
                .filter(map_location_links::role.eq(role_for_delete)),
        )
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot detach location: {error}")))
    })
    .await?;
    if deleted == 0 {
        return Err(UnifiedError::not_found("Location link not found"));
    }
    ctx.hooks.emit(
        events::MAPS_LOCATION_DETACHED,
        json!({
            "tenant_id": tenant_id.to_string(),
            "entity_type": entity_type,
            "entity_id": entity_id.to_string(),
            "location_id": location_id.to_string(),
            "role": role,
        }),
    );
    Ok(())
}

/// All locations attached to one record (public contract), in
/// label order, each with the role it was attached under.
pub async fn locations_for_entity(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
) -> UnifiedResult<Vec<EntityLocation>> {
    validate_entity_type(entity_type)?;
    let pool = db_pool(ctx)?;
    // Own the borrowed string: the blocking closure below
    // must be 'static (spawn_blocking), so it cannot borrow
    // the caller's parameter.
    let entity_type = entity_type.to_string();
    let links = run_blocking(pool.clone(), move |connection| {
        map_location_links::table
            .filter(map_location_links::tenant_id.eq(tenant_id))
            .filter(map_location_links::entity_type.eq(entity_type))
            .filter(map_location_links::entity_id.eq(entity_id))
            .select(LinkRow::as_select())
            .load::<LinkRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list location links: {error}")))
    })
    .await?;
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let location_ids: Vec<Uuid> = links.iter().map(|link| link.location_id).collect();
    let rows = run_blocking(pool, move |connection| {
        map_locations::table
            .filter(map_locations::tenant_id.eq(tenant_id))
            .filter(map_locations::id.eq_any(location_ids))
            .select(LocationRow::as_select())
            .load::<LocationRow>(connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load linked locations: {error}"))
            })
    })
    .await?;
    let mut by_id: std::collections::HashMap<Uuid, MapLocation> = rows
        .into_iter()
        .map(|row| (row.id, MapLocation::from(row)))
        .collect();
    let mut found: Vec<EntityLocation> = links
        .into_iter()
        .filter_map(|link| {
            by_id
                .remove(&link.location_id)
                .map(|location| EntityLocation {
                    location,
                    role: link.role,
                })
        })
        .collect();
    found.sort_by(|a, b| a.location.label.cmp(&b.location.label));
    Ok(found)
}

// Layers.

pub(crate) async fn list_layers(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
) -> UnifiedResult<Vec<MapLayer>> {
    let pool = db_pool(ctx)?;
    let rows = run_blocking(pool, move |connection| {
        map_layers::table
            .filter(map_layers::tenant_id.eq(tenant_id))
            .order((map_layers::name.asc(), map_layers::id.asc()))
            .select(LayerRow::as_select())
            .load::<LayerRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list map layers: {error}")))
    })
    .await?;
    Ok(rows.into_iter().map(MapLayer::from).collect())
}

pub(crate) async fn create_layer(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    actor: &str,
    input: CreateLayerInput,
) -> UnifiedResult<MapLayer> {
    let name = validate_text(&input.name, "name", 200)?;
    let description = validate_optional_text(&input.description, "description", 1000)?;
    validate_json_object(&input.style, "style")?;
    let pool = db_pool(ctx)?;
    let row = LayerRow {
        id: Uuid::now_v7(),
        tenant_id,
        name,
        description,
        visible: input.visible,
        style: input.style,
        created_by: actor.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_layers::table)
            .values(&stored)
            .execute(connection)
            .map_err(|error| map_db_error(error, "Map layer"))
    })
    .await?;
    Ok(MapLayer::from(row))
}

pub(crate) async fn update_layer(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
    input: UpdateLayerInput,
) -> UnifiedResult<MapLayer> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_layer_row(connection, tenant_id, id)
    })
    .await?;
    let name = match &input.name {
        Some(value) => validate_text(value, "name", 200)?,
        None => current.name.clone(),
    };
    let description = match &input.description {
        Some(value) => validate_optional_text(value, "description", 1000)?,
        None => current.description.clone(),
    };
    let visible = input.visible.unwrap_or(current.visible);
    let style = match &input.style {
        Some(value) => {
            validate_json_object(value, "style")?;
            value.clone()
        }
        None => current.style.clone(),
    };
    let now = Utc::now();
    let updated = run_blocking(pool, move |connection| {
        diesel::update(
            map_layers::table
                .filter(map_layers::tenant_id.eq(tenant_id))
                .filter(map_layers::id.eq(id)),
        )
        .set((
            map_layers::name.eq(&name),
            map_layers::description.eq(&description),
            map_layers::visible.eq(visible),
            map_layers::style.eq(&style),
            map_layers::updated_at.eq(now),
        ))
        .execute(connection)
        .map_err(|error| map_db_error(error, "Map layer"))?;
        load_layer_row(connection, tenant_id, id)
    })
    .await?;
    Ok(MapLayer::from(updated))
}

pub(crate) async fn delete_layer(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<MapLayer> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_layer_row(connection, tenant_id, id)
    })
    .await?;
    run_blocking(pool, move |connection| {
        diesel::delete(
            map_layers::table
                .filter(map_layers::tenant_id.eq(tenant_id))
                .filter(map_layers::id.eq(id)),
        )
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot delete map layer: {error}")))?;
        Ok(())
    })
    .await?;
    Ok(MapLayer::from(current))
}

// Geofences.

pub(crate) async fn list_geofences(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
) -> UnifiedResult<Vec<MapGeofence>> {
    let pool = db_pool(ctx)?;
    let rows = run_blocking(pool, move |connection| {
        map_geofences::table
            .filter(map_geofences::tenant_id.eq(tenant_id))
            .order((map_geofences::name.asc(), map_geofences::id.asc()))
            .select(GeofenceRow::as_select())
            .load::<GeofenceRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list map geofences: {error}")))
    })
    .await?;
    Ok(rows.into_iter().map(MapGeofence::from).collect())
}

pub(crate) async fn create_geofence(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    actor: &str,
    input: CreateGeofenceInput,
) -> UnifiedResult<MapGeofence> {
    let name = validate_text(&input.name, "name", 200)?;
    let description = validate_optional_text(&input.description, "description", 1000)?;
    validate_geometry(&input.geometry)?;
    let pool = db_pool(ctx)?;
    let row = GeofenceRow {
        id: Uuid::now_v7(),
        tenant_id,
        name,
        description,
        geometry: input.geometry,
        created_by: actor.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_geofences::table)
            .values(&stored)
            .execute(connection)
            .map_err(|error| map_db_error(error, "Map geofence"))
    })
    .await?;
    Ok(MapGeofence::from(row))
}

pub(crate) async fn get_geofence(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<MapGeofence> {
    let pool = db_pool(ctx)?;
    let row = run_blocking(pool, move |connection| {
        load_geofence_row(connection, tenant_id, id)
    })
    .await?;
    Ok(MapGeofence::from(row))
}

pub(crate) async fn update_geofence(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
    input: UpdateGeofenceInput,
) -> UnifiedResult<MapGeofence> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_geofence_row(connection, tenant_id, id)
    })
    .await?;
    let name = match &input.name {
        Some(value) => validate_text(value, "name", 200)?,
        None => current.name.clone(),
    };
    let description = match &input.description {
        Some(value) => validate_optional_text(value, "description", 1000)?,
        None => current.description.clone(),
    };
    let geometry = match &input.geometry {
        Some(value) => {
            validate_geometry(value)?;
            value.clone()
        }
        None => current.geometry.clone(),
    };
    let now = Utc::now();
    let updated = run_blocking(pool, move |connection| {
        diesel::update(
            map_geofences::table
                .filter(map_geofences::tenant_id.eq(tenant_id))
                .filter(map_geofences::id.eq(id)),
        )
        .set((
            map_geofences::name.eq(&name),
            map_geofences::description.eq(&description),
            map_geofences::geometry.eq(&geometry),
            map_geofences::updated_at.eq(now),
        ))
        .execute(connection)
        .map_err(|error| map_db_error(error, "Map geofence"))?;
        load_geofence_row(connection, tenant_id, id)
    })
    .await?;
    Ok(MapGeofence::from(updated))
}

pub(crate) async fn delete_geofence(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<MapGeofence> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_geofence_row(connection, tenant_id, id)
    })
    .await?;
    run_blocking(pool, move |connection| {
        diesel::delete(
            map_geofences::table
                .filter(map_geofences::tenant_id.eq(tenant_id))
                .filter(map_geofences::id.eq(id)),
        )
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot delete map geofence: {error}")))?;
        Ok(())
    })
    .await?;
    Ok(MapGeofence::from(current))
}

/// Test one point against one zone (public contract).
pub async fn within_geofence(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    geofence_id: Uuid,
    latitude: f64,
    longitude: f64,
) -> UnifiedResult<bool> {
    validate_coordinates(latitude, longitude)?;
    let fence = get_geofence(ctx, tenant_id, geofence_id).await?;
    Ok(point_in_geometry(longitude, latitude, &fence.geometry))
}

/// Every zone of this tenant that contains the point (public
/// contract). The geometry is evaluated in Rust, never in SQL.
pub async fn geofences_containing(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    latitude: f64,
    longitude: f64,
) -> UnifiedResult<Vec<MapGeofence>> {
    validate_coordinates(latitude, longitude)?;
    let fences = list_geofences(ctx, tenant_id).await?;
    Ok(fences
        .into_iter()
        .filter(|fence| point_in_geometry(longitude, latitude, &fence.geometry))
        .collect())
}

// Positions (tracking history).

/// Record one GPS position (public contract). The position is
/// stored, `maps.position_recorded` is emitted, and every
/// geofence of the tenant is evaluated against the previous
/// and the new position: entering a zone emits
/// `maps.geofence_entered`, leaving one emits
/// `maps.geofence_exited`. A tracker that batches fixes sends
/// the time each fix was taken in `recorded_at`.
pub async fn record_position(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    input: RecordPositionInput,
) -> UnifiedResult<MapPosition> {
    validate_entity_type(&input.entity_type)?;
    validate_coordinates(input.latitude, input.longitude)?;
    if let Some(accuracy) = input.accuracy_m {
        if !accuracy.is_finite() || accuracy < 0.0 {
            return Err(UnifiedError::bad_request("accuracy_m must be zero or more"));
        }
    }
    if let Some(speed) = input.speed_kmh {
        if !speed.is_finite() || speed < 0.0 {
            return Err(UnifiedError::bad_request("speed_kmh must be zero or more"));
        }
    }
    if let Some(heading) = input.heading_deg {
        if !heading.is_finite() || !(0.0..360.0).contains(&heading) {
            return Err(UnifiedError::bad_request(
                "heading_deg must be between 0 (inclusive) and 360 (exclusive)",
            ));
        }
    }
    let source = match &input.source {
        Some(value) => validate_text(value, "source", 64)?,
        None => "manual".to_string(),
    };
    let recorded_at = input.recorded_at.unwrap_or_else(Utc::now);
    let pool = db_pool(ctx)?;
    // The previous fix for this entity: the latest recorded
    // position before this one, so enter/exit is evaluated
    // against movement, not against the new row itself.
    let previous = run_blocking(pool.clone(), {
        let entity_type = input.entity_type.clone();
        let entity_id = input.entity_id;
        move |connection| {
            map_positions::table
                .filter(map_positions::tenant_id.eq(tenant_id))
                .filter(map_positions::entity_type.eq(&entity_type))
                .filter(map_positions::entity_id.eq(entity_id))
                .filter(map_positions::recorded_at.lt(recorded_at))
                .order((
                    map_positions::recorded_at.desc(),
                    map_positions::created_at.desc(),
                    map_positions::id.desc(),
                ))
                .select(PositionRow::as_select())
                .first::<PositionRow>(connection)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load previous position: {error}"))
                })
        }
    })
    .await?;
    let row = PositionRow {
        id: Uuid::now_v7(),
        tenant_id,
        entity_type: input.entity_type.clone(),
        entity_id: input.entity_id,
        latitude: input.latitude,
        longitude: input.longitude,
        accuracy_m: input.accuracy_m,
        speed_kmh: input.speed_kmh,
        heading_deg: input.heading_deg,
        source,
        recorded_at,
        created_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_positions::table)
            .values(&stored)
            .execute(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot record position: {error}")))?;
        Ok(())
    })
    .await?;
    let position = MapPosition::from(row);
    ctx.hooks.emit(
        events::MAPS_POSITION_RECORDED,
        json!({
            "tenant_id": tenant_id.to_string(),
            "entity_type": position.entity_type,
            "entity_id": position.entity_id.to_string(),
            "latitude": position.latitude,
            "longitude": position.longitude,
            "recorded_at": position.recorded_at.to_rfc3339(),
        }),
    );
    let fences = list_geofences(ctx, tenant_id).await?;
    for fence in fences {
        let now_inside = point_in_geometry(position.longitude, position.latitude, &fence.geometry);
        let was_inside = previous
            .as_ref()
            .map(|prev| point_in_geometry(prev.longitude, prev.latitude, &fence.geometry));
        let payload = json!({
            "tenant_id": tenant_id.to_string(),
            "entity_type": position.entity_type,
            "entity_id": position.entity_id.to_string(),
            "geofence_id": fence.id.to_string(),
            "geofence_name": fence.name,
            "latitude": position.latitude,
            "longitude": position.longitude,
            "recorded_at": position.recorded_at.to_rfc3339(),
        });
        match (was_inside, now_inside) {
            (Some(false), true) | (None, true) => {
                ctx.hooks.emit(events::MAPS_GEOFENCE_ENTERED, payload);
            }
            (Some(true), false) => {
                ctx.hooks.emit(events::MAPS_GEOFENCE_EXITED, payload);
            }
            _ => {}
        }
    }
    Ok(position)
}

/// Position history for one entity, newest first. Filters:
/// `from` / `to` bound `recorded_at`. Paginated with the
/// shared `page` / `per_page`.
pub(crate) async fn list_positions(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    params: &PaginationParams,
    entity_type: &str,
    entity_id: Uuid,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> UnifiedResult<PageResult<MapPosition>> {
    validate_entity_type(entity_type)?;
    let pagination = params.normalized();
    let offset = params.offset();
    let limit = params.limit();
    let pool = db_pool(ctx)?;
    let entity = entity_type.to_string();
    let total = run_blocking(pool.clone(), move |connection| {
        let mut query = map_positions::table
            .filter(map_positions::tenant_id.eq(tenant_id))
            .filter(map_positions::entity_type.eq(&entity))
            .filter(map_positions::entity_id.eq(entity_id))
            .into_boxed();
        if let Some(from) = from {
            query = query.filter(map_positions::recorded_at.ge(from));
        }
        if let Some(to) = to {
            query = query.filter(map_positions::recorded_at.le(to));
        }
        query
            .count()
            .get_result::<i64>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot count positions: {error}")))
    })
    .await?;
    let entity = entity_type.to_string();
    let rows = run_blocking(pool, move |connection| {
        let mut query = map_positions::table
            .filter(map_positions::tenant_id.eq(tenant_id))
            .filter(map_positions::entity_type.eq(&entity))
            .filter(map_positions::entity_id.eq(entity_id))
            .into_boxed();
        if let Some(from) = from {
            query = query.filter(map_positions::recorded_at.ge(from));
        }
        if let Some(to) = to {
            query = query.filter(map_positions::recorded_at.le(to));
        }
        query
            .order((
                map_positions::recorded_at.desc(),
                map_positions::created_at.desc(),
                map_positions::id.desc(),
            ))
            .offset(offset)
            .limit(limit)
            .select(PositionRow::as_select())
            .load::<PositionRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list positions: {error}")))
    })
    .await?;
    Ok(PageResult::new(
        rows.into_iter().map(MapPosition::from).collect(),
        total as u64,
        &pagination,
    ))
}

// Location kinds (admin-definable definitions).

/// All kinds for the tenant: the built-ins first, overlaid by
/// the tenant's own rows (a tenant row with the same slug wins
/// over the built-in). Disabled kinds stay listed with
/// `enabled: false` so the admin UI can re-enable them.
pub(crate) async fn list_kinds(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
) -> UnifiedResult<Vec<LocationKind>> {
    let pool = db_pool(ctx)?;
    let rows = run_blocking(pool, move |connection| {
        map_location_kinds::table
            .filter(map_location_kinds::tenant_id.eq(tenant_id))
            .order((map_location_kinds::slug.asc(), map_location_kinds::id.asc()))
            .select(KindRow::as_select())
            .load::<KindRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list location kinds: {error}")))
    })
    .await?;
    let mut kinds = builtin_location_kinds();
    for row in rows {
        let defined = LocationKind::from(row);
        if let Some(existing) = kinds.iter_mut().find(|kind| kind.slug == defined.slug) {
            *existing = defined;
        } else {
            kinds.push(defined);
        }
    }
    Ok(kinds)
}

pub(crate) async fn create_kind(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    input: CreateKindInput,
) -> UnifiedResult<LocationKind> {
    validate_slug(&input.slug, "slug")?;
    let label = validate_text(&input.label, "label", 200)?;
    let icon = match &input.icon {
        Some(value) => {
            validate_slug(value, "icon")?;
            value.clone()
        }
        None => "pin".to_string(),
    };
    let color = match &input.color {
        Some(value) => {
            validate_color(value)?;
            value.clone()
        }
        None => "#0B6B3A".to_string(),
    };
    let pool = db_pool(ctx)?;
    let row = KindRow {
        id: Uuid::now_v7(),
        tenant_id,
        slug: input.slug,
        label,
        icon,
        color,
        enabled: input.enabled,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_location_kinds::table)
            .values(&stored)
            .execute(connection)
            .map_err(|error| map_db_error(error, "Location kind"))
    })
    .await?;
    Ok(LocationKind::from(row))
}

pub(crate) async fn update_kind(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
    input: UpdateKindInput,
) -> UnifiedResult<LocationKind> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_kind_row(connection, tenant_id, id)
    })
    .await?;
    // The slug is immutable: a kind is renamed by disabling it
    // and creating a new one, so existing locations never change
    // kind under the admin's feet.
    let label = match &input.label {
        Some(value) => validate_text(value, "label", 200)?,
        None => current.label.clone(),
    };
    let icon = match &input.icon {
        Some(value) => {
            validate_slug(value, "icon")?;
            value.clone()
        }
        None => current.icon.clone(),
    };
    let color = match &input.color {
        Some(value) => {
            validate_color(value)?;
            value.clone()
        }
        None => current.color.clone(),
    };
    let enabled = input.enabled.unwrap_or(current.enabled);
    let now = Utc::now();
    let updated = run_blocking(pool, move |connection| {
        diesel::update(
            map_location_kinds::table
                .filter(map_location_kinds::tenant_id.eq(tenant_id))
                .filter(map_location_kinds::id.eq(id)),
        )
        .set((
            map_location_kinds::label.eq(&label),
            map_location_kinds::icon.eq(&icon),
            map_location_kinds::color.eq(&color),
            map_location_kinds::enabled.eq(enabled),
            map_location_kinds::updated_at.eq(now),
        ))
        .execute(connection)
        .map_err(|error| map_db_error(error, "Location kind"))?;
        load_kind_row(connection, tenant_id, id)
    })
    .await?;
    Ok(LocationKind::from(updated))
}

pub(crate) async fn delete_kind(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<LocationKind> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_kind_row(connection, tenant_id, id)
    })
    .await?;
    run_blocking(pool, move |connection| {
        diesel::delete(
            map_location_kinds::table
                .filter(map_location_kinds::tenant_id.eq(tenant_id))
                .filter(map_location_kinds::id.eq(id)),
        )
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot delete location kind: {error}")))?;
        Ok(())
    })
    .await?;
    // Deleting a tenant override reveals the built-in with the
    // same slug again; locations that used the kind keep their
    // stored kind string either way.
    Ok(LocationKind::from(current))
}

// Link roles (admin-definable definitions).

/// All roles for the tenant: built-ins overlaid by the
/// tenant's own rows, same shape as `list_kinds`.
pub(crate) async fn list_roles(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
) -> UnifiedResult<Vec<LinkRole>> {
    let pool = db_pool(ctx)?;
    let rows = run_blocking(pool, move |connection| {
        map_link_roles::table
            .filter(map_link_roles::tenant_id.eq(tenant_id))
            .order((map_link_roles::slug.asc(), map_link_roles::id.asc()))
            .select(RoleRow::as_select())
            .load::<RoleRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list link roles: {error}")))
    })
    .await?;
    let mut roles = builtin_link_roles();
    for row in rows {
        let defined = LinkRole::from(row);
        if let Some(existing) = roles.iter_mut().find(|role| role.slug == defined.slug) {
            *existing = defined;
        } else {
            roles.push(defined);
        }
    }
    Ok(roles)
}

pub(crate) async fn create_role(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    input: CreateRoleInput,
) -> UnifiedResult<LinkRole> {
    validate_slug(&input.slug, "slug")?;
    let label = validate_text(&input.label, "label", 200)?;
    let pool = db_pool(ctx)?;
    let row = RoleRow {
        id: Uuid::now_v7(),
        tenant_id,
        slug: input.slug,
        label,
        enabled: input.enabled,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_link_roles::table)
            .values(&stored)
            .execute(connection)
            .map_err(|error| map_db_error(error, "Link role"))
    })
    .await?;
    Ok(LinkRole::from(row))
}

pub(crate) async fn update_role(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
    input: UpdateRoleInput,
) -> UnifiedResult<LinkRole> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_role_row(connection, tenant_id, id)
    })
    .await?;
    let label = match &input.label {
        Some(value) => validate_text(value, "label", 200)?,
        None => current.label.clone(),
    };
    let enabled = input.enabled.unwrap_or(current.enabled);
    let now = Utc::now();
    let updated = run_blocking(pool, move |connection| {
        diesel::update(
            map_link_roles::table
                .filter(map_link_roles::tenant_id.eq(tenant_id))
                .filter(map_link_roles::id.eq(id)),
        )
        .set((
            map_link_roles::label.eq(&label),
            map_link_roles::enabled.eq(enabled),
            map_link_roles::updated_at.eq(now),
        ))
        .execute(connection)
        .map_err(|error| map_db_error(error, "Link role"))?;
        load_role_row(connection, tenant_id, id)
    })
    .await?;
    Ok(LinkRole::from(updated))
}

pub(crate) async fn delete_role(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<LinkRole> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_role_row(connection, tenant_id, id)
    })
    .await?;
    run_blocking(pool, move |connection| {
        diesel::delete(
            map_link_roles::table
                .filter(map_link_roles::tenant_id.eq(tenant_id))
                .filter(map_link_roles::id.eq(id)),
        )
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot delete link role: {error}")))?;
        Ok(())
    })
    .await?;
    Ok(LinkRole::from(current))
}

// Providers (registry) and settings.

/// All providers visible to the tenant: the built-ins, overlaid
/// by the platform rows, overlaid by the tenant's own rows (a
/// tenant row with the same slug wins). A disabled provider
/// stays listed with `enabled: false`.
pub(crate) async fn list_providers(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
) -> UnifiedResult<Vec<MapProvider>> {
    let pool = db_pool(ctx)?;
    let platform_rows = run_blocking(pool.clone(), move |connection| {
        map_providers::table
            .filter(map_providers::tenant_id.is_null())
            .order((map_providers::slug.asc(), map_providers::id.asc()))
            .select(ProviderRow::as_select())
            .load::<ProviderRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list map providers: {error}")))
    })
    .await?;
    let tenant_rows = run_blocking(pool, move |connection| {
        map_providers::table
            .filter(map_providers::tenant_id.eq(tenant_id))
            .order((map_providers::slug.asc(), map_providers::id.asc()))
            .select(ProviderRow::as_select())
            .load::<ProviderRow>(connection)
            .map_err(|error| UnifiedError::internal(format!("Cannot list map providers: {error}")))
    })
    .await?;
    let mut providers = builtin_providers();
    for row in platform_rows.into_iter().chain(tenant_rows) {
        let defined = MapProvider::from(row);
        if let Some(existing) = providers.iter_mut().find(|p| p.slug == defined.slug) {
            *existing = defined;
        } else {
            providers.push(defined);
        }
    }
    Ok(providers)
}

/// Find one visible provider by slug, or `None`. A disabled
/// provider is still found: callers decide whether disabled is
/// an error (settings save) or a listing detail.
async fn find_provider(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    slug: &str,
) -> UnifiedResult<Option<MapProvider>> {
    let providers = list_providers(ctx, tenant_id).await?;
    Ok(providers.into_iter().find(|provider| provider.slug == slug))
}

pub(crate) async fn create_provider(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    input: CreateProviderInput,
) -> UnifiedResult<MapProvider> {
    validate_slug(&input.slug, "slug")?;
    let display_name = validate_text(&input.display_name, "display_name", 200)?;
    validate_tile_template(&input.tile_url_template)?;
    let attribution = match &input.attribution {
        Some(value) => validate_text(value, "attribution", 500)?,
        None => String::new(),
    };
    let max_zoom = input.max_zoom.unwrap_or(19);
    validate_max_zoom(max_zoom)?;
    let subdomains = input.subdomains.unwrap_or_default();
    validate_subdomains(&subdomains)?;
    validate_json_object(&input.options, "options")?;
    let pool = db_pool(ctx)?;
    let row = ProviderRow {
        id: Uuid::now_v7(),
        tenant_id: Some(tenant_id),
        slug: input.slug,
        display_name,
        tile_url_template: input.tile_url_template.trim().to_string(),
        attribution,
        requires_api_key: input.requires_api_key,
        max_zoom,
        subdomains,
        options: input.options,
        enabled: input.enabled,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_providers::table)
            .values(&stored)
            .execute(connection)
            .map_err(|error| map_db_error(error, "Map provider"))
    })
    .await?;
    Ok(MapProvider::from(row))
}

pub(crate) async fn update_provider(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
    input: UpdateProviderInput,
) -> UnifiedResult<MapProvider> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_provider_row(connection, tenant_id, id)
    })
    .await?;
    // A platform-shared provider is read-only except for its
    // `enabled` flag: one tenant must not rewrite the tile URL
    // every other tenant renders from. A tenant that needs a
    // different template creates its own provider row with the
    // same slug instead (the tenant row shadows the platform
    // one).
    if current.tenant_id.is_none() {
        let forbidden_field = input.display_name.is_some()
            || input.tile_url_template.is_some()
            || input.attribution.is_some()
            || input.requires_api_key.is_some()
            || input.max_zoom.is_some()
            || input.subdomains.is_some()
            || input.options.is_some();
        if forbidden_field {
            return Err(UnifiedError::forbidden(
                "A platform provider can only be enabled or disabled; create a tenant provider with the same slug to override its template",
            ));
        }
        let enabled = input.enabled.unwrap_or(current.enabled);
        let now = Utc::now();
        let updated = run_blocking(pool, move |connection| {
            diesel::update(
                map_providers::table
                    .filter(map_providers::tenant_id.is_null())
                    .filter(map_providers::id.eq(id)),
            )
            .set((
                map_providers::enabled.eq(enabled),
                map_providers::updated_at.eq(now),
            ))
            .execute(connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot update map provider: {error}"))
            })?;
            load_provider_row(connection, tenant_id, id)
        })
        .await?;
        return Ok(MapProvider::from(updated));
    }
    let display_name = match &input.display_name {
        Some(value) => validate_text(value, "display_name", 200)?,
        None => current.display_name.clone(),
    };
    let tile_url_template = match &input.tile_url_template {
        Some(value) => {
            validate_tile_template(value)?;
            value.trim().to_string()
        }
        None => current.tile_url_template.clone(),
    };
    let attribution = match &input.attribution {
        Some(value) => validate_text(value, "attribution", 500)?,
        None => current.attribution.clone(),
    };
    let requires_api_key = input.requires_api_key.unwrap_or(current.requires_api_key);
    let max_zoom = match input.max_zoom {
        Some(value) => {
            validate_max_zoom(value)?;
            value
        }
        None => current.max_zoom,
    };
    let subdomains = match &input.subdomains {
        Some(value) => {
            validate_subdomains(value)?;
            value.clone()
        }
        None => current.subdomains.clone(),
    };
    let options = match &input.options {
        Some(value) => {
            validate_json_object(value, "options")?;
            value.clone()
        }
        None => current.options.clone(),
    };
    let enabled = input.enabled.unwrap_or(current.enabled);
    let now = Utc::now();
    let updated = run_blocking(pool, move |connection| {
        diesel::update(
            map_providers::table
                .filter(map_providers::tenant_id.eq(tenant_id))
                .filter(map_providers::id.eq(id)),
        )
        .set((
            map_providers::display_name.eq(&display_name),
            map_providers::tile_url_template.eq(&tile_url_template),
            map_providers::attribution.eq(&attribution),
            map_providers::requires_api_key.eq(requires_api_key),
            map_providers::max_zoom.eq(max_zoom),
            map_providers::subdomains.eq(&subdomains),
            map_providers::options.eq(&options),
            map_providers::enabled.eq(enabled),
            map_providers::updated_at.eq(now),
        ))
        .execute(connection)
        .map_err(|error| map_db_error(error, "Map provider"))?;
        load_provider_row(connection, tenant_id, id)
    })
    .await?;
    Ok(MapProvider::from(updated))
}

pub(crate) async fn delete_provider(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    id: Uuid,
) -> UnifiedResult<MapProvider> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        load_provider_row(connection, tenant_id, id)
    })
    .await?;
    // A platform provider is never deleted: disable it instead,
    // so the seeded catalogue stays intact for every tenant.
    if current.tenant_id.is_none() {
        return Err(UnifiedError::forbidden(
            "A platform provider cannot be deleted; disable it instead",
        ));
    }
    run_blocking(pool, move |connection| {
        diesel::delete(
            map_providers::table
                .filter(map_providers::tenant_id.eq(tenant_id))
                .filter(map_providers::id.eq(id)),
        )
        .execute(connection)
        .map_err(|error| UnifiedError::internal(format!("Cannot delete map provider: {error}")))?;
        Ok(())
    })
    .await?;
    Ok(MapProvider::from(current))
}

/// The tenant's settings as the API returns them: the key is
/// never included, only whether one is stored.
pub(crate) async fn get_settings_view(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
) -> UnifiedResult<MapSettingsView> {
    let pool = db_pool(ctx)?;
    let row = run_blocking(pool, move |connection| {
        map_settings::table
            .filter(map_settings::tenant_id.eq(tenant_id))
            .select(SettingsRow::as_select())
            .first::<SettingsRow>(connection)
            .optional()
            .map_err(|error| UnifiedError::internal(format!("Cannot load map settings: {error}")))
    })
    .await?;
    Ok(match row {
        Some(row) => MapSettingsView {
            tenant_id,
            active_provider: row.active_provider,
            has_api_key: row
                .api_key_ciphertext
                .as_deref()
                .is_some_and(|v| !v.is_empty()),
            default_latitude: row.default_latitude,
            default_longitude: row.default_longitude,
            default_zoom: row.default_zoom,
        },
        None => MapSettingsView {
            tenant_id,
            active_provider: PROVIDER_OPENSTREETMAP.to_string(),
            has_api_key: false,
            default_latitude: 0.0,
            default_longitude: 0.0,
            default_zoom: 2,
        },
    })
}

/// Save the tenant's settings. `api_key` is write-only: an
/// omitted field keeps the stored key, an empty string clears
/// it, and a value is encrypted before it is stored (fail
/// closed when `app.secret_key` is not configured). The active
/// provider must be a known, enabled provider.
pub(crate) async fn save_settings(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
    input: SaveSettingsInput,
) -> UnifiedResult<MapSettingsView> {
    let pool = db_pool(ctx)?;
    let current = run_blocking(pool.clone(), move |connection| {
        map_settings::table
            .filter(map_settings::tenant_id.eq(tenant_id))
            .select(SettingsRow::as_select())
            .first::<SettingsRow>(connection)
            .optional()
            .map_err(|error| UnifiedError::internal(format!("Cannot load map settings: {error}")))
    })
    .await?;
    let active_provider = match &input.active_provider {
        Some(value) => {
            validate_slug(value, "active_provider")?;
            value.clone()
        }
        None => current
            .as_ref()
            .map(|row| row.active_provider.clone())
            .unwrap_or_else(|| PROVIDER_OPENSTREETMAP.to_string()),
    };
    match find_provider(ctx, tenant_id, &active_provider).await? {
        Some(provider) if provider.enabled => {}
        Some(_) => {
            return Err(UnifiedError::bad_request(format!(
                "Map provider \"{active_provider}\" is disabled"
            )));
        }
        None => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown map provider \"{active_provider}\""
            )));
        }
    }
    let default_latitude = match input.default_latitude {
        Some(value) => {
            super::validation::validate_latitude(value, "default_latitude")?;
            value
        }
        None => current
            .as_ref()
            .map(|row| row.default_latitude)
            .unwrap_or(0.0),
    };
    let default_longitude = match input.default_longitude {
        Some(value) => {
            super::validation::validate_longitude(value, "default_longitude")?;
            value
        }
        None => current
            .as_ref()
            .map(|row| row.default_longitude)
            .unwrap_or(0.0),
    };
    let default_zoom = match input.default_zoom {
        Some(value) => {
            if !(0..=19).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "default_zoom must be between 0 and 19",
                ));
            }
            value
        }
        None => current.as_ref().map(|row| row.default_zoom).unwrap_or(2),
    };
    let api_key_ciphertext = match &input.api_key {
        None => current.and_then(|row| row.api_key_ciphertext),
        Some(value) if value.is_empty() => None,
        Some(value) => Some(encrypt_secret(value)?),
    };
    let now = Utc::now();
    let row = SettingsRow {
        tenant_id,
        active_provider,
        api_key_ciphertext,
        default_latitude,
        default_longitude,
        default_zoom,
        created_at: now,
        updated_at: now,
    };
    let stored = row.clone();
    run_blocking(pool, move |connection| {
        diesel::insert_into(map_settings::table)
            .values(&stored)
            .on_conflict(map_settings::tenant_id)
            .do_update()
            .set((
                map_settings::active_provider.eq(&stored.active_provider),
                map_settings::api_key_ciphertext.eq(&stored.api_key_ciphertext),
                map_settings::default_latitude.eq(stored.default_latitude),
                map_settings::default_longitude.eq(stored.default_longitude),
                map_settings::default_zoom.eq(stored.default_zoom),
                map_settings::updated_at.eq(now),
            ))
            .execute(connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot save map settings: {error}"))
            })?;
        Ok(())
    })
    .await?;
    Ok(MapSettingsView {
        tenant_id,
        active_provider: row.active_provider,
        has_api_key: row
            .api_key_ciphertext
            .as_deref()
            .is_some_and(|v| !v.is_empty()),
        default_latitude: row.default_latitude,
        default_longitude: row.default_longitude,
        default_zoom: row.default_zoom,
    })
}

/// Resolve the map configuration the client renders from: the
/// active provider's tile URL with `{key}` substituted when a
/// browser key is stored. `tile_url` is `None` when the
/// provider needs a key and none is stored.
pub(crate) async fn resolve_settings(
    ctx: &Arc<AppContext>,
    tenant_id: Uuid,
) -> UnifiedResult<ResolvedMapSettings> {
    let view = get_settings_view(ctx, tenant_id).await?;
    let provider = match find_provider(ctx, tenant_id, &view.active_provider).await? {
        Some(provider) if provider.enabled => provider,
        _ => builtin_providers()
            .into_iter()
            .find(|provider| provider.slug == PROVIDER_OPENSTREETMAP)
            .expect("built-in OpenStreetMap provider exists"),
    };
    let api_key = if provider.requires_api_key {
        let pool = db_pool(ctx)?;
        let ciphertext = run_blocking(pool, move |connection| {
            map_settings::table
                .filter(map_settings::tenant_id.eq(tenant_id))
                .select(map_settings::api_key_ciphertext)
                .first::<Option<String>>(connection)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load map settings: {error}"))
                })
        })
        .await?
        .flatten();
        decrypt_optional(&ciphertext)?
    } else {
        None
    };
    let key_configured = api_key.is_some();
    Ok(ResolvedMapSettings {
        active_provider: provider.slug.clone(),
        display_name: provider.display_name.clone(),
        tile_url: resolve_tile_url(
            &provider.tile_url_template,
            api_key.as_deref(),
            provider.requires_api_key,
        ),
        attribution: provider.attribution.clone(),
        requires_api_key: provider.requires_api_key,
        key_configured,
        max_zoom: provider.max_zoom,
        subdomains: parse_subdomains(&provider.subdomains),
        options: provider.options.clone(),
        default_latitude: view.default_latitude,
        default_longitude: view.default_longitude,
        default_zoom: view.default_zoom,
    })
}
