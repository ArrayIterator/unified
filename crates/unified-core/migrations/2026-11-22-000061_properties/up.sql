-- Properties module tables for the Unified Pentagonal Project
-- (`src/modules/properties.rs`): land parcels / housing
-- developments, site plans (master plans), blocks / plots for
-- sale with an audited status machine, and block history.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` / `created_by` / `assigned_customer_id` /
--     `actor_id` columns are TEXT with NO foreign key on purpose:
--     actor ids in this codebase are opaque strings (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table. The users module stays the source of
--     truth for identity; properties only reference users by id
--     and never duplicate user rows. The same applies to
--     `assigned_customer_id`: a CRM customer reference only, never
--     a duplicated customer row.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE: deleting a company
--     removes its properties (and, through the cascades below,
--     their plans, blocks, and history).
--   - Money is NUMERIC(19,4): `properties` carries no money;
--     `property_blocks.price` is the sale price. Areas are
--     NUMERIC(19,4) square metres, lengths NUMERIC(19,4) metres.
--   - Geometry is JSONB (GeoJSON geometry objects only:
--     Polygon / MultiPolygon / LineString, validated by the
--     module before write). `geometry_hash` is the SHA-256 hex of
--     the canonical geometry JSON, for change detection.
--   - Block status is VARCHAR with a CHECK constraint listing the
--     five statuses: available, reserved, sold, assigned,
--     blocked. The status machine lives in the module; the CHECK
--     only guards the stored value.
--   - `property_block_history` is append-only audit: one row per
--     status transition, including admin corrections. Nothing in
--     the module deletes history except deleting the block (or
--     its property) itself.
--   - Block purchases (`BlockPurchase` in the module: the record
--     linking a block to a store order id) have NO table in this
--     migration: they live in memory only today. A persistence
--     table for purchases is a follow-up; until it exists, a
--     restart loses the in-flight purchase -> order linkage and
--     the payment listeners cannot resolve captures for blocks
--     sold before the restart.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One property: a land parcel or a housing development.
CREATE TABLE IF NOT EXISTS properties (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    description TEXT,
    slug VARCHAR(128) NOT NULL UNIQUE,
    latitude DOUBLE PRECISION CHECK (latitude BETWEEN -90 AND 90),
    longitude DOUBLE PRECISION CHECK (longitude BETWEEN -180 AND 180),
    address VARCHAR(500),
    total_area_sqm NUMERIC(19,4) CHECK (total_area_sqm >= 0),
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    status VARCHAR(20) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'sold_out', 'archived')),
    cover_storage_target VARCHAR(200),
    cover_storage_key VARCHAR(512),
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((latitude IS NULL) = (longitude IS NULL)),
    CHECK ((cover_storage_target IS NULL) = (cover_storage_key IS NULL))
);

CREATE INDEX IF NOT EXISTS idx_properties_status
    ON properties (status);
CREATE INDEX IF NOT EXISTS idx_properties_company_id
    ON properties (company_id);
CREATE INDEX IF NOT EXISTS idx_properties_slug
    ON properties (slug);

-- One site plan (master plan) of a property. `geometry` is the plan
-- boundary (Polygon / MultiPolygon GeoJSON, or NULL when the plan
-- has no boundary yet).
CREATE TABLE IF NOT EXISTS property_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    property_id UUID NOT NULL REFERENCES properties (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR(200) NOT NULL,
    description TEXT,
    geometry JSONB,
    geometry_hash VARCHAR(64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((geometry IS NULL) = (geometry_hash IS NULL))
);

CREATE INDEX IF NOT EXISTS idx_property_plans_property_id
    ON property_plans (property_id);

-- One block (plot) inside a site plan. `geometry` is GeoJSON
-- (Polygon / MultiPolygon for a plot, LineString for a drawn line).
-- `area_sqm` is computed server-side for polygons, `length_m`
-- for lines.
CREATE TABLE IF NOT EXISTS property_blocks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID NOT NULL REFERENCES property_plans (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    property_id UUID NOT NULL REFERENCES properties (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title VARCHAR(200) NOT NULL,
    notes TEXT,
    geometry JSONB NOT NULL,
    geometry_hash VARCHAR(64) NOT NULL,
    area_sqm NUMERIC(19,4) CHECK (area_sqm >= 0),
    length_m NUMERIC(19,4) CHECK (length_m >= 0),
    price NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (price >= 0),
    currency VARCHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR(20) NOT NULL DEFAULT 'available'
        CHECK (status IN ('available', 'reserved', 'sold', 'assigned', 'blocked')),
    assigned_customer_id TEXT,
    click_behavior VARCHAR(10) NOT NULL DEFAULT 'popup'
        CHECK (click_behavior IN ('popup', 'link')),
    click_link_kind VARCHAR(10)
        CHECK (click_link_kind IN ('product', 'order', 'page')),
    click_link_target TEXT,
    order_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((click_behavior = 'link') = (click_link_kind IS NOT NULL)),
    CHECK ((click_behavior = 'link') = (click_link_target IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS idx_property_blocks_plan_id_status
    ON property_blocks (plan_id, status);
CREATE INDEX IF NOT EXISTS idx_property_blocks_property_id
    ON property_blocks (property_id);
CREATE INDEX IF NOT EXISTS idx_property_blocks_assigned_customer_id
    ON property_blocks (assigned_customer_id);
CREATE INDEX IF NOT EXISTS idx_property_blocks_order_id
    ON property_blocks (order_id);

-- One audited block status change: who moved the block, when, from
-- which status to which, and why. Append-only.
CREATE TABLE IF NOT EXISTS property_block_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    block_id UUID NOT NULL REFERENCES property_blocks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    property_id UUID NOT NULL REFERENCES properties (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    plan_id UUID NOT NULL REFERENCES property_plans (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    from_status VARCHAR(20) NOT NULL
        CHECK (from_status IN ('available', 'reserved', 'sold', 'assigned', 'blocked')),
    to_status VARCHAR(20) NOT NULL
        CHECK (to_status IN ('available', 'reserved', 'sold', 'assigned', 'blocked')),
    actor_id TEXT NOT NULL,
    reason TEXT,
    order_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (from_status <> to_status)
);

CREATE INDEX IF NOT EXISTS idx_property_block_history_block_id_created_at
    ON property_block_history (block_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_property_block_history_property_id
    ON property_block_history (property_id);
