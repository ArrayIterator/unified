// Diesel schema for the Map & Tagging domain of the
// unified-db-platform bounded context.
//
// Byte-identical to migration
// 2026-11-23-000134_map_tagging. The file is named `maps.rs`
// (not `map_locations.rs`) so the `pub mod maps` binding never
// collides with a table module: the tables are `map_locations`,
// `map_location_links`, `map_layers`, `map_geofences`,
// `map_positions`, `map_location_kinds`, `map_link_roles`,
// `map_providers`, and `map_settings` (see the name-collision
// rule in `src/schema.rs`).

diesel::table! {
    map_layers (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        visible -> Bool,
        style -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    map_locations (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        layer_id -> Nullable<Uuid>,
        label -> Varchar,
        kind -> Varchar,
        latitude -> Double,
        longitude -> Double,
        address -> Nullable<Text>,
        metadata -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    map_location_links (tenant_id, entity_type, entity_id, location_id, role) {
        tenant_id -> Uuid,
        entity_type -> Varchar,
        entity_id -> Uuid,
        location_id -> Uuid,
        role -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    map_geofences (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        geometry -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    map_positions (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        entity_type -> Varchar,
        entity_id -> Uuid,
        latitude -> Double,
        longitude -> Double,
        accuracy_m -> Nullable<Double>,
        speed_kmh -> Nullable<Double>,
        heading_deg -> Nullable<Double>,
        source -> Varchar,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    map_location_kinds (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        slug -> Varchar,
        label -> Varchar,
        icon -> Varchar,
        color -> Varchar,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    map_link_roles (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        slug -> Varchar,
        label -> Varchar,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    map_providers (id) {
        id -> Uuid,
        tenant_id -> Nullable<Uuid>,
        slug -> Varchar,
        display_name -> Varchar,
        tile_url_template -> Text,
        attribution -> Varchar,
        requires_api_key -> Bool,
        max_zoom -> Int4,
        subdomains -> Varchar,
        options -> Jsonb,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    map_settings (tenant_id) {
        tenant_id -> Uuid,
        active_provider -> Varchar,
        api_key_ciphertext -> Nullable<Text>,
        default_latitude -> Double,
        default_longitude -> Double,
        default_zoom -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::joinable!(map_locations -> map_layers (layer_id));
diesel::joinable!(map_location_links -> map_locations (location_id));

diesel::allow_tables_to_appear_in_same_query!(
    map_layers,
    map_locations,
    map_location_links,
    map_geofences,
    map_positions,
    map_location_kinds,
    map_link_roles,
    map_providers,
    map_settings,
);
