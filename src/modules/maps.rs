//! Map & Tagging — the single shared geospatial module.
//!
//! Locations (pins), generic location links for ANY module's
//! records, layers, geofences, GPS tracking history, and a
//! configurable tile-provider registry. Other modules consume
//! it through the public contract in `maps::store`
//! (`attach_location`, `locations_for_entity`, `nearby`,
//! `within_geofence`, `record_position`) or the HTTP API under
//! `/api/v1/maps`, and declare the dependency with
//! `requires() -> &["maps"]`. The full model, contract, and
//! dependency rules live in docs/integration/MAP_TAGGING.md.
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes live
//! in `maps/module.rs`.

mod crypto;
mod handlers;
mod helpers;
pub mod module;
mod providers;
mod store;
pub mod types;
mod validation;

pub use self::module::{register, MapsModule};
pub use self::store::{
    attach_location, detach_location, geofences_containing, locations_for_entity, nearby,
    record_position, within_geofence,
};
pub use self::types::{
    ContainsInput, ContainsResult, CreateGeofenceInput, CreateKindInput, CreateLayerInput,
    CreateLocationInput, CreateProviderInput, CreateRoleInput, EntityLocation, LinkRole,
    LocationKind, LocationLink, MapGeofence, MapLayer, MapLocation, MapPosition, MapProvider,
    MapSettingsView, NearbyLocation, RecordPositionInput, ResolvedMapSettings, SaveSettingsInput,
    UpdateGeofenceInput, UpdateKindInput, UpdateLayerInput, UpdateLocationInput,
    UpdateProviderInput, UpdateRoleInput, CAP_MAPS_MANAGE, CAP_MAPS_TRACK, CAP_MAPS_VIEW,
    MAX_NEARBY_RADIUS_KM, MAX_NEARBY_RESULTS, MODULE_ID, PROVIDER_GOOGLE, PROVIDER_MAPBOX,
    PROVIDER_OPENSTREETMAP,
};
