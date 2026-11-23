//! Map & Tagging — shared types: locations, links, layers,
//! geofences, positions, kinds, roles, providers, settings, and
//! the hook + capability names other modules use to talk to this
//! module.
//!
//! This module is the SINGLE shared geospatial store. No other
//! module keeps its own ad-hoc location table: a module that
//! needs a location attaches one of these rows through
//! `map_location_links` (see `store::attach_location`).
//!
//! Location kinds and link roles are DATA, never a hard-coded
//! enum: the built-ins below are the fallback a tenant gets
//! when it has defined none of its own, and the admin-editable
//! definitions in `map_location_kinds` / `map_link_roles`
//! override or extend them (a tenant row with the same slug
//! wins over the built-in). Providers are data the same way
//! (see `providers.rs`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Stable module id. Consumers declare it in `requires()`:
/// `&["maps"]` — see docs/integration/MAP_TAGGING.md. The id is
/// the dependency id: enabling a consumer auto-activates this
/// module, and disabling it is blocked while an enabled consumer
/// still needs it (the extensions dependency system enforces
/// both; this module is also core, so it cannot be disabled at
/// all).
pub const MODULE_ID: &str = "maps";

/// Capability to read locations, layers, geofences, positions,
/// kinds, roles, providers, and the resolved map settings.
pub const CAP_MAPS_VIEW: &str = "maps.view";

/// Capability to create, update, and delete locations, links,
/// layers, geofences, kinds, roles, providers, and settings.
pub const CAP_MAPS_MANAGE: &str = "maps.manage";

/// Capability to record GPS positions (tracking history). Kept
/// separate from `maps.manage` so a GPS tracker / IoT agent can
/// be granted position writes without full map administration.
pub const CAP_MAPS_TRACK: &str = "maps.track";

/// Default tile provider: OpenStreetMap, free, no API key.
pub const PROVIDER_OPENSTREETMAP: &str = "osm";

/// Built-in paid tile provider: Mapbox, needs a browser access token.
pub const PROVIDER_MAPBOX: &str = "mapbox";

/// Built-in paid tile provider: Google Maps, needs a
/// referrer-restricted browser API key (see `providers.rs`).
pub const PROVIDER_GOOGLE: &str = "google";

/// Hard cap on locations returned by one nearby search: a
/// nearby search is a map viewport, not a bulk export.
pub const MAX_NEARBY_RESULTS: usize = 200;

/// Hard cap on the radius of a nearby search, in kilometres.
pub const MAX_NEARBY_RADIUS_KM: f64 = 500.0;

/// Hard cap on positions returned by one history page: the
/// caller pages with `page` / `per_page` for more.
pub const MAX_POSITION_HISTORY: i64 = 100;

/// Largest free-form JSON blob (metadata / style / geometry
/// serialised) accepted in one request, in bytes.
pub const MAX_JSON_BLOB_BYTES: usize = 16 * 1024;

/// One stored location (a pin): a label, a kind, coordinates,
/// an optional address, free-form metadata, and an optional
/// layer. The coordinates are the truth; the address is display
/// text only and is never geocoded by this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapLocation {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub layer_id: Option<Uuid>,
    pub label: String,
    pub kind: String,
    pub latitude: f64,
    pub longitude: f64,
    pub address: Option<String>,
    pub metadata: Value,
    pub created_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One location plus its distance from a query point, as
/// returned by the nearby search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearbyLocation {
    #[serde(flatten)]
    pub location: MapLocation,
    pub distance_km: f64,
}

/// One generic link: a location attached to another module's
/// record. `entity_type` is the owning module's record kind
/// (`shipment`, `warehouse`, `customer`, `odp`, `branch`,
/// `product`, ...); `entity_id` is that record's id; `role`
/// is what the location is for that record (`home`, `office`,
/// `delivery`, `asset`, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationLink {
    pub tenant_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub location_id: Uuid,
    pub role: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One location attached to an entity, with the role it was
/// attached under — the shape `locations_for_entity` returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityLocation {
    #[serde(flatten)]
    pub location: MapLocation,
    pub role: String,
}

/// One named pin layer: visibility plus a free-form style
/// object (colour, icon set, ...). A location belongs to at
/// most one layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapLayer {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub visible: bool,
    pub style: Value,
    pub created_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One named geofence zone. `geometry` is GeoJSON: a Polygon
/// or MultiPolygon of `[longitude, latitude]` positions. The
/// runtime evaluates point-in-zone (ray casting); the database
/// never interprets the geometry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapGeofence {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub geometry: Value,
    pub created_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One timestamped GPS position for one entity: the tracking
/// history GPS trackers and IoT devices write to. `source`
/// names the writer (`gps`, `iot`, `manual`, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapPosition {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub heading_deg: Option<f64>,
    pub source: String,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One location-kind definition. `is_builtin` is true for the
/// built-in fallback kinds (see `builtin_location_kinds`):
/// they have no stored row and cannot be edited or deleted,
/// only overridden by a tenant row with the same slug.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationKind {
    pub id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub slug: String,
    pub label: String,
    pub icon: String,
    pub color: String,
    pub enabled: bool,
    #[serde(default)]
    pub is_builtin: bool,
}

/// One link-role definition, same shape as `LocationKind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRole {
    pub id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub slug: String,
    pub label: String,
    pub enabled: bool,
    #[serde(default)]
    pub is_builtin: bool,
}

/// One tile provider definition. `tenant_id` is `None` for a
/// platform-shared definition (the seeded OpenStreetMap,
/// Mapbox, and Google rows); a tenant row overrides / extends
/// the catalogue for that tenant only. The definition never
/// carries a key: whether a browser API key is required is a
/// flag, and the key itself lives encrypted in `map_settings`
/// (see `crypto.rs`). `subdomains` is the comma-separated
/// `{s}` list the tile client rotates through (for example
/// `abc` for OpenStreetMap); `options` is a free-form object
/// of extra tile-layer options (for example a Mapbox style id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapProvider {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub slug: String,
    pub display_name: String,
    pub tile_url_template: String,
    pub attribution: String,
    pub requires_api_key: bool,
    pub max_zoom: i32,
    pub subdomains: String,
    pub options: Value,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// The tenant's map settings as the API returns them. The API
/// key is NEVER returned: `has_api_key` only reports whether
/// one is stored. The resolved tile URL (with the key
/// substituted) is served by `GET /api/v1/maps/settings` as
/// `ResolvedMapSettings`, not here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapSettingsView {
    pub tenant_id: Uuid,
    pub active_provider: String,
    pub has_api_key: bool,
    pub default_latitude: f64,
    pub default_longitude: f64,
    pub default_zoom: i32,
}

/// The resolved map configuration the React map renders from:
/// the active provider's tile URL with `{z}` / `{x}` / `{y}`
/// (and `{key}` when a browser key is stored) already
/// substituted, so the client never builds a provider URL
/// itself. `{s}` is left in place for the tile client, which
/// rotates it through `subdomains`. `tile_url` is `None` when
/// the active provider needs a key and none is stored: the map
/// then shows an honest "provider needs an API key" note
/// instead of broken tiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedMapSettings {
    pub active_provider: String,
    pub display_name: String,
    pub tile_url: Option<String>,
    pub attribution: String,
    pub requires_api_key: bool,
    pub key_configured: bool,
    pub max_zoom: i32,
    pub subdomains: Vec<String>,
    pub options: Value,
    pub default_latitude: f64,
    pub default_longitude: f64,
    pub default_zoom: i32,
}

/// The built-in location kinds: the fallback a tenant gets
/// when it has defined none of its own. A tenant row with the
/// same slug overrides the built-in (same label/icon/colour
/// shape, own values).
pub fn builtin_location_kinds() -> Vec<LocationKind> {
    let kind = |slug: &str, label: &str, icon: &str, color: &str| LocationKind {
        id: None,
        tenant_id: None,
        slug: slug.to_string(),
        label: label.to_string(),
        icon: icon.to_string(),
        color: color.to_string(),
        enabled: true,
        is_builtin: true,
    };
    vec![
        kind("pin", "Pin", "pin", "#0B6B3A"),
        kind("home", "Home", "home", "#0B6B3A"),
        kind("office", "Office", "office", "#1D4ED8"),
        kind("warehouse", "Warehouse", "warehouse", "#92400E"),
        kind("store", "Store", "store", "#7C3AED"),
        kind("customer", "Customer", "customer", "#0E7490"),
        kind("asset", "Asset", "asset", "#475569"),
        kind("vehicle", "Vehicle", "vehicle", "#BE123C"),
        kind("odp", "ODP", "odp", "#B45309"),
        kind("branch", "Branch", "branch", "#15803D"),
        kind(
            "delivery-point",
            "Delivery point",
            "delivery-point",
            "#0369A1",
        ),
    ]
}

/// The built-in link roles: the fallback a tenant gets when it
/// has defined none of its own.
pub fn builtin_link_roles() -> Vec<LinkRole> {
    let role = |slug: &str, label: &str| LinkRole {
        id: None,
        tenant_id: None,
        slug: slug.to_string(),
        label: label.to_string(),
        enabled: true,
        is_builtin: true,
    };
    vec![
        role("home", "Home"),
        role("office", "Office"),
        role("delivery", "Delivery"),
        role("asset", "Asset"),
        role("billing", "Billing"),
        role("shipping", "Shipping"),
        role("pickup", "Pickup"),
    ]
}

/// The built-in provider definitions, mirroring the seeded
/// `map_providers` rows in migration 000134. The runtime reads
/// the table, never this list, for the live catalogue; this
/// list is the fallback when the table has no row for a slug
/// (for example before the migration has run).
pub fn builtin_providers() -> Vec<MapProvider> {
    let now = chrono::Utc::now();
    vec![
        MapProvider {
            id: Uuid::nil(),
            tenant_id: None,
            slug: PROVIDER_OPENSTREETMAP.to_string(),
            display_name: "OpenStreetMap".to_string(),
            tile_url_template: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".to_string(),
            attribution: "© OpenStreetMap contributors".to_string(),
            requires_api_key: false,
            max_zoom: 19,
            subdomains: "abc".to_string(),
            options: Value::Object(Default::default()),
            enabled: true,
            created_at: now,
            updated_at: now,
        },
        MapProvider {
            id: Uuid::nil(),
            tenant_id: None,
            slug: PROVIDER_MAPBOX.to_string(),
            display_name: "Mapbox".to_string(),
            tile_url_template: "https://api.mapbox.com/styles/v1/mapbox/streets-v12/tiles/{z}/{x}/{y}?access_token={key}"
                .to_string(),
            attribution: "© Mapbox © OpenStreetMap".to_string(),
            requires_api_key: true,
            max_zoom: 22,
            subdomains: String::new(),
            options: Value::Object(Default::default()),
            enabled: true,
            created_at: now,
            updated_at: now,
        },
        MapProvider {
            id: Uuid::nil(),
            tenant_id: None,
            slug: PROVIDER_GOOGLE.to_string(),
            display_name: "Google Maps".to_string(),
            tile_url_template:
                "https://mt1.google.com/vt/lyrs=m&x={x}&y={y}&z={z}&key={key}".to_string(),
            attribution: "© Google".to_string(),
            requires_api_key: true,
            max_zoom: 20,
            subdomains: String::new(),
            options: Value::Object(Default::default()),
            enabled: true,
            created_at: now,
            updated_at: now,
        },
    ]
}

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

/// `POST /api/v1/maps/locations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLocationInput {
    pub label: String,
    pub kind: String,
    pub latitude: f64,
    pub longitude: f64,
    pub address: Option<String>,
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub metadata: Value,
}

/// `PUT /api/v1/maps/locations/{id}`: every field is optional;
/// an omitted field keeps its stored value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLocationInput {
    pub label: Option<String>,
    pub kind: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<Option<String>>,
    pub layer_id: Option<Option<Uuid>>,
    pub metadata: Option<Value>,
}

/// `POST /api/v1/maps/links` (attach) and the detach body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationLinkInput {
    pub entity_type: String,
    pub entity_id: Uuid,
    pub location_id: Uuid,
    pub role: String,
}

/// `POST /api/v1/maps/layers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLayerInput {
    pub name: String,
    pub description: Option<String>,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub style: Value,
}

fn default_visible() -> bool {
    true
}

/// `PUT /api/v1/maps/layers/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLayerInput {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub visible: Option<bool>,
    pub style: Option<Value>,
}

/// `POST /api/v1/maps/geofences`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGeofenceInput {
    pub name: String,
    pub description: Option<String>,
    pub geometry: Value,
}

/// `PUT /api/v1/maps/geofences/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateGeofenceInput {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub geometry: Option<Value>,
}

/// `POST /api/v1/maps/geofences/{id}/contains`: test one point
/// against one zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainsInput {
    pub latitude: f64,
    pub longitude: f64,
}

/// The answer to a contains test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainsResult {
    pub geofence_id: Uuid,
    pub name: String,
    pub contains: bool,
}

/// `POST /api/v1/maps/positions`: record one GPS position.
/// `recorded_at` defaults to now; a tracker that batches
/// positions sends the time the fix was taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordPositionInput {
    pub entity_type: String,
    pub entity_id: Uuid,
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub heading_deg: Option<f64>,
    pub source: Option<String>,
    pub recorded_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `POST /api/v1/maps/kinds`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKindInput {
    pub slug: String,
    pub label: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    #[serde(default = "default_visible")]
    pub enabled: bool,
}

/// `PUT /api/v1/maps/kinds/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateKindInput {
    pub label: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub enabled: Option<bool>,
}

/// `POST /api/v1/maps/roles`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRoleInput {
    pub slug: String,
    pub label: String,
    #[serde(default = "default_visible")]
    pub enabled: bool,
}

/// `PUT /api/v1/maps/roles/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRoleInput {
    pub label: Option<String>,
    pub enabled: Option<bool>,
}

/// `POST /api/v1/maps/providers`. A custom provider is any
/// tile source: an open-source tile server, Mapbox, Google
/// Maps, or a fully custom tile API — the same shape carries
/// them all (tile URL template + attribution + key flag + max
/// zoom + subdomains + options). `subdomains` is a string of
/// single-character subdomain names (`abc`); `options` is a
/// free-form object of extra tile-layer options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProviderInput {
    pub slug: String,
    pub display_name: String,
    pub tile_url_template: String,
    pub attribution: Option<String>,
    #[serde(default)]
    pub requires_api_key: bool,
    pub max_zoom: Option<i32>,
    pub subdomains: Option<String>,
    #[serde(default)]
    pub options: Value,
    #[serde(default = "default_visible")]
    pub enabled: bool,
}

/// `PUT /api/v1/maps/providers/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateProviderInput {
    pub display_name: Option<String>,
    pub tile_url_template: Option<String>,
    pub attribution: Option<String>,
    pub requires_api_key: Option<bool>,
    pub max_zoom: Option<i32>,
    pub subdomains: Option<String>,
    pub options: Option<Value>,
    pub enabled: Option<bool>,
}

/// `PUT /api/v1/maps/settings`. `api_key` is write-only: an
/// empty string clears the stored key, an omitted field keeps
/// it, and the key is never returned by any endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveSettingsInput {
    pub active_provider: Option<String>,
    pub api_key: Option<String>,
    pub default_latitude: Option<f64>,
    pub default_longitude: Option<f64>,
    pub default_zoom: Option<i32>,
}
