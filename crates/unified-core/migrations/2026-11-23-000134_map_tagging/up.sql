-- 000134: Map & Tagging shared geospatial module (task 358).
--
-- The single shared location store every other module consumes:
-- IoT / GPS trackers, logistics, ISP ODP and customer pins,
-- customer / branch / product / shipping locations. No module
-- keeps its own ad-hoc latitude/longitude store: a module that
-- needs a location attaches one of these rows through
-- `map_location_links` instead of adding columns of its own.
--
-- Tables, all tenant-scoped by `tenant_id` (the same partition
-- key the runtime stores carry; the tables are provisioned per
-- tenant schema, so the column also stays correct when a
-- deployment shares one schema across tenants):
--
--   * map_locations — a pin: label, kind, latitude/longitude,
--     address, free-form metadata JSONB, optional layer.
--   * map_location_links — the generic join: ANY module attaches
--     a location to its records with (entity_type, entity_id,
--     location_id, role). `entity_type` is the owning module's
--     record kind ('shipment', 'warehouse', 'customer', 'odp',
--     'branch', 'product', ...); `role` is 'home' / 'office' /
--     'delivery' / 'asset' / ... — see `map_link_roles`.
--   * map_layers — named pin layers (visibility + style JSONB);
--     a location belongs to at most one layer.
--   * map_geofences — named zones stored as GeoJSON Polygon /
--     MultiPolygon in `geometry`; point-in-zone is evaluated by
--     the runtime (ray casting), never by the database.
--   * map_positions — timestamped GPS positions for one entity
--     (entity_type + entity_id): the tracking history GPS
--     trackers and IoT devices write to, and the geofence
--     enter/exit evaluation reads from.
--   * map_location_kinds — admin-definable location kinds
--     (slug, label, icon, colour). Built-in kinds are a
--     runtime fallback, never rows: a tenant with no rows still
--     gets the built-ins (see docs/integration/MAP_TAGGING.md).
--   * map_link_roles — admin-definable link roles, same shape.
--   * map_providers — tile provider definitions. `tenant_id`
--     NULL is a platform-shared definition (the three seeded
--     rows below); a tenant row overrides / extends the
--     catalogue for that tenant only.
--   * map_settings — one row per tenant: the active provider,
--     the encrypted browser API key (ciphertext only, written
--     through the settings endpoint, never returned), and the
--     default map centre / zoom.
--
-- The runtime (src/modules/maps) is Diesel-backed, like the
-- widgets module; the Diesel schema in
-- crates/unified-db-platform/src/schema/maps.rs mirrors these
-- tables exactly. No migration has ever run on live PostgreSQL;
-- every statement is IF NOT EXISTS, so a re-run is a no-op.

CREATE TABLE IF NOT EXISTS map_layers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) <= 1000),
    visible BOOLEAN NOT NULL DEFAULT TRUE,
    style JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_map_layers_tenant_name
    ON map_layers (tenant_id, name);

CREATE TABLE IF NOT EXISTS map_locations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    layer_id UUID REFERENCES map_layers (id) ON DELETE SET NULL,
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    kind TEXT NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    latitude DOUBLE PRECISION NOT NULL CHECK (latitude BETWEEN -90 AND 90),
    longitude DOUBLE PRECISION NOT NULL CHECK (longitude BETWEEN -180 AND 180),
    address TEXT CHECK (address IS NULL OR char_length(address) <= 1000),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_map_locations_tenant_kind
    ON map_locations (tenant_id, kind);
CREATE INDEX IF NOT EXISTS idx_map_locations_tenant_layer
    ON map_locations (tenant_id, layer_id);
CREATE INDEX IF NOT EXISTS idx_map_locations_tenant_coords
    ON map_locations (tenant_id, latitude, longitude);

CREATE TABLE IF NOT EXISTS map_location_links (
    tenant_id UUID NOT NULL,
    entity_type TEXT NOT NULL CHECK (char_length(entity_type) BETWEEN 1 AND 64),
    entity_id UUID NOT NULL,
    location_id UUID NOT NULL REFERENCES map_locations (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (char_length(role) BETWEEN 1 AND 64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, entity_type, entity_id, location_id, role)
);

CREATE INDEX IF NOT EXISTS idx_map_location_links_location
    ON map_location_links (tenant_id, location_id);
CREATE INDEX IF NOT EXISTS idx_map_location_links_entity
    ON map_location_links (tenant_id, entity_type, entity_id);

CREATE TABLE IF NOT EXISTS map_geofences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) <= 1000),
    geometry JSONB NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_map_geofences_tenant_name
    ON map_geofences (tenant_id, name);

CREATE TABLE IF NOT EXISTS map_positions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    entity_type TEXT NOT NULL CHECK (char_length(entity_type) BETWEEN 1 AND 64),
    entity_id UUID NOT NULL,
    latitude DOUBLE PRECISION NOT NULL CHECK (latitude BETWEEN -90 AND 90),
    longitude DOUBLE PRECISION NOT NULL CHECK (longitude BETWEEN -180 AND 180),
    accuracy_m DOUBLE PRECISION CHECK (accuracy_m IS NULL OR accuracy_m >= 0),
    speed_kmh DOUBLE PRECISION CHECK (speed_kmh IS NULL OR speed_kmh >= 0),
    heading_deg DOUBLE PRECISION CHECK (heading_deg IS NULL OR (heading_deg >= 0 AND heading_deg < 360)),
    source TEXT NOT NULL DEFAULT 'manual' CHECK (char_length(source) BETWEEN 1 AND 64),
    recorded_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_map_positions_tenant_entity_recorded
    ON map_positions (tenant_id, entity_type, entity_id, recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_map_positions_tenant_recorded
    ON map_positions (tenant_id, recorded_at DESC);

CREATE TABLE IF NOT EXISTS map_location_kinds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    icon TEXT NOT NULL DEFAULT 'pin' CHECK (char_length(icon) BETWEEN 1 AND 64),
    color TEXT NOT NULL DEFAULT '#0B6B3A' CHECK (color ~ '^#[0-9a-fA-F]{6}$'),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_map_location_kinds_tenant_slug
    ON map_location_kinds (tenant_id, slug);

CREATE TABLE IF NOT EXISTS map_link_roles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_map_link_roles_tenant_slug
    ON map_link_roles (tenant_id, slug);

CREATE TABLE IF NOT EXISTS map_providers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID,
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    display_name TEXT NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 200),
    tile_url_template TEXT NOT NULL CHECK (char_length(tile_url_template) BETWEEN 1 AND 500),
    attribution TEXT NOT NULL DEFAULT '' CHECK (char_length(attribution) <= 500),
    requires_api_key BOOLEAN NOT NULL DEFAULT FALSE,
    max_zoom INTEGER NOT NULL DEFAULT 19 CHECK (max_zoom BETWEEN 0 AND 22),
    subdomains TEXT NOT NULL DEFAULT '' CHECK (char_length(subdomains) <= 200),
    options JSONB NOT NULL DEFAULT '{}'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- tenant_id NULL is the platform-shared definition, so plain
-- (tenant_id, slug) uniqueness would let two platform rows share
-- a slug (NULL is never equal to NULL). COALESCE the NULL to the
-- nil UUID so the platform catalogue stays unique per slug too.
CREATE UNIQUE INDEX IF NOT EXISTS idx_map_providers_tenant_slug
    ON map_providers ((COALESCE(tenant_id, '00000000-0000-0000-0000-000000000000'::uuid)), slug);

CREATE TABLE IF NOT EXISTS map_settings (
    tenant_id UUID PRIMARY KEY,
    active_provider TEXT NOT NULL DEFAULT 'osm' CHECK (char_length(active_provider) BETWEEN 1 AND 64),
    api_key_ciphertext TEXT,
    default_latitude DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (default_latitude BETWEEN -90 AND 90),
    default_longitude DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (default_longitude BETWEEN -180 AND 180),
    default_zoom INTEGER NOT NULL DEFAULT 2 CHECK (default_zoom BETWEEN 0 AND 19),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Seeded platform provider definitions (tenant_id NULL: shared
-- by every tenant, editable / disableable through the providers
-- API). No API key lives here, in code, or in any seed: the
-- Mapbox and Google rows only declare that a browser key is
-- required, and an admin supplies one per tenant through PUT
-- /api/v1/maps/settings. Any other provider — any open-source
-- tile server or a fully custom tile API — is a provider row an
-- admin creates through POST /api/v1/maps/providers with the
-- same shape (tile URL template + attribution + key flag +
-- max zoom + subdomains + options).
INSERT INTO map_providers (tenant_id, slug, display_name, tile_url_template, attribution, requires_api_key, max_zoom, subdomains, options, enabled)
VALUES (
    NULL,
    'osm',
    'OpenStreetMap',
    'https://tile.openstreetmap.org/{z}/{x}/{y}.png',
    '© OpenStreetMap contributors',
    FALSE,
    19,
    'abc',
    '{}'::jsonb,
    TRUE
) ON CONFLICT ((COALESCE(tenant_id, '00000000-0000-0000-0000-000000000000'::uuid)), slug) DO NOTHING;

INSERT INTO map_providers (tenant_id, slug, display_name, tile_url_template, attribution, requires_api_key, max_zoom, subdomains, options, enabled)
VALUES (
    NULL,
    'mapbox',
    'Mapbox',
    'https://api.mapbox.com/styles/v1/mapbox/streets-v12/tiles/{z}/{x}/{y}?access_token={key}',
    '© Mapbox © OpenStreetMap',
    TRUE,
    22,
    '',
    '{}'::jsonb,
    TRUE
) ON CONFLICT ((COALESCE(tenant_id, '00000000-0000-0000-0000-000000000000'::uuid)), slug) DO NOTHING;

INSERT INTO map_providers (tenant_id, slug, display_name, tile_url_template, attribution, requires_api_key, max_zoom, subdomains, options, enabled)
VALUES (
    NULL,
    'google',
    'Google Maps',
    'https://mt1.google.com/vt/lyrs=m&x={x}&y={y}&z={z}&key={key}',
    '© Google',
    TRUE,
    20,
    '',
    '{}'::jsonb,
    TRUE
) ON CONFLICT ((COALESCE(tenant_id, '00000000-0000-0000-0000-000000000000'::uuid)), slug) DO NOTHING;
