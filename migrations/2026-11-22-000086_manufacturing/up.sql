-- Manufacturing / MRP tables for the Unified Pentagonal Project
-- (`src/modules/manufacturing.rs`): bills of materials with
-- versions, work centers, work orders, production runs, material
-- reservations, scrap reason codes, units of measure, and stock
-- notices received from the inventory side.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/manufacturing.rs`,
-- which stay authoritative until the migration is executed and
-- verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `mfg_boms.product_id` and
--     `mfg_bom_version_lines.component_product_id` are plain
--     VARCHAR product references, not foreign keys: products
--     live in the catalogue / assets side and are identified by
--     string id, exactly like `pos_sales.cashier_id` in
--     migration 000076. A component may itself be a manufactured
--     product; multi-level resolution happens in the module.
--   - `mfg_boms.active_version_id` is likewise a plain UUID
--     reference to `mfg_bom_versions.id`, not a foreign key:
--     the BOM row is created before its first version row, so
--     the reference is enforced in the module, not in SQL.
--   - BOM versions are immutable rows: editing a BOM inserts a
--     new `mfg_bom_versions` row (with its lines and routing
--     rows) and moves `mfg_boms.active_version_id` to it.
--     `mfg_bom_versions.version` is unique per BOM.
--   - `mfg_work_orders.status` is `draft`, `confirmed`,
--     `in_progress`, `done`, or `cancelled`; the allowed
--     transitions live in the module settings, not in a CHECK,
--     so an admin can change the state machine without a
--     migration. `inspection_status` is `passed`, `failed`, or
--     NULL until the quality side answers.
--   - Money is NUMERIC(19,4). `mfg_work_centers.cost_per_hour`
--     is NULL when no labor rate is configured; the cost
--     roll-up reports that as a gap instead of guessing.
--   - `mfg_work_order_requirements` is the per-order snapshot
--     of the pinned BOM version at creation time, including the
--     scrap allowance in `quantity_required`.
--   - `mfg_reservations.status` is `pending_confirmation`,
--     `reserved`, `partially_consumed`, `consumed`, `released`,
--     or `short`. Stock itself is never stored here; this table
--     records what was requested of the inventory side.
--   - `mfg_scrap_reasons` and `mfg_units` are admin-editable
--     definition tables seeded with the same defaults as the
--     module; `mfg_production_runs.scrap_reason_id` is NULL
--     unless the run recorded scrap.
--   - `mfg_stock_notices` is the audit log of
--     `manufacturing.stock.confirmed` / `manufacturing.stock.short`
--     events received from the inventory side.

CREATE TABLE IF NOT EXISTS mfg_boms (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id VARCHAR(64) NOT NULL,
    name VARCHAR(200) NOT NULL,
    code VARCHAR(64) NOT NULL UNIQUE,
    description TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'draft', 'archived')),
    active_version_id UUID,
    latest_version INTEGER NOT NULL DEFAULT 1 CHECK (latest_version >= 1),
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_mfg_boms_product_id ON mfg_boms (product_id);
CREATE INDEX IF NOT EXISTS idx_mfg_boms_status ON mfg_boms (status);

CREATE TABLE IF NOT EXISTS mfg_bom_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bom_id UUID NOT NULL REFERENCES mfg_boms (id),
    version INTEGER NOT NULL CHECK (version >= 1),
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (bom_id, version)
);
CREATE INDEX IF NOT EXISTS idx_mfg_bom_versions_bom_id ON mfg_bom_versions (bom_id);

CREATE TABLE IF NOT EXISTS mfg_bom_version_lines (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bom_version_id UUID NOT NULL REFERENCES mfg_bom_versions (id),
    component_product_id VARCHAR(64) NOT NULL,
    quantity NUMERIC(19, 4) NOT NULL CHECK (quantity > 0),
    unit VARCHAR(32) NOT NULL,
    scrap_allowance_percent NUMERIC(5, 2) NOT NULL DEFAULT 0
        CHECK (scrap_allowance_percent >= 0 AND scrap_allowance_percent <= 100),
    notes TEXT,
    UNIQUE (bom_version_id, component_product_id)
);
CREATE INDEX IF NOT EXISTS idx_mfg_bom_version_lines_version_id
    ON mfg_bom_version_lines (bom_version_id);

CREATE TABLE IF NOT EXISTS mfg_work_centers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    code VARCHAR(64) NOT NULL UNIQUE,
    capacity_hours_per_day NUMERIC(19, 4) NOT NULL DEFAULT 8
        CHECK (capacity_hours_per_day > 0),
    cost_per_hour NUMERIC(19, 4) CHECK (cost_per_hour IS NULL OR cost_per_hour >= 0),
    currency CHAR(3),
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'inactive')),
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_mfg_work_centers_status ON mfg_work_centers (status);

CREATE TABLE IF NOT EXISTS mfg_bom_routing_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bom_version_id UUID NOT NULL REFERENCES mfg_bom_versions (id),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    name VARCHAR(200) NOT NULL,
    work_center_id UUID NOT NULL REFERENCES mfg_work_centers (id),
    duration_minutes NUMERIC(19, 4) NOT NULL CHECK (duration_minutes > 0),
    description TEXT,
    UNIQUE (bom_version_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_mfg_bom_routing_steps_version_id
    ON mfg_bom_routing_steps (bom_version_id);
CREATE INDEX IF NOT EXISTS idx_mfg_bom_routing_steps_work_center_id
    ON mfg_bom_routing_steps (work_center_id);

CREATE TABLE IF NOT EXISTS mfg_work_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    number VARCHAR(32) NOT NULL UNIQUE,
    bom_id UUID NOT NULL REFERENCES mfg_boms (id),
    bom_version_id UUID NOT NULL REFERENCES mfg_bom_versions (id),
    bom_version INTEGER NOT NULL CHECK (bom_version >= 1),
    product_id VARCHAR(64) NOT NULL,
    quantity_planned NUMERIC(19, 4) NOT NULL CHECK (quantity_planned > 0),
    quantity_produced NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (quantity_produced >= 0),
    quantity_scrapped NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (quantity_scrapped >= 0),
    status VARCHAR(20) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'confirmed', 'in_progress', 'done', 'cancelled')),
    planned_start TIMESTAMPTZ,
    planned_end TIMESTAMPTZ,
    actual_start TIMESTAMPTZ,
    actual_end TIMESTAMPTZ,
    company_id VARCHAR(64),
    notes TEXT,
    inspection_status VARCHAR(20)
        CHECK (inspection_status IS NULL OR inspection_status IN ('passed', 'failed', 'unknown')),
    inspection_notes TEXT,
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (planned_end IS NULL OR planned_start IS NULL OR planned_end >= planned_start)
);
CREATE INDEX IF NOT EXISTS idx_mfg_work_orders_bom_id ON mfg_work_orders (bom_id);
CREATE INDEX IF NOT EXISTS idx_mfg_work_orders_product_id ON mfg_work_orders (product_id);
CREATE INDEX IF NOT EXISTS idx_mfg_work_orders_status ON mfg_work_orders (status);
CREATE INDEX IF NOT EXISTS idx_mfg_work_orders_created_at ON mfg_work_orders (created_at);

CREATE TABLE IF NOT EXISTS mfg_work_order_requirements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id UUID NOT NULL REFERENCES mfg_work_orders (id),
    component_product_id VARCHAR(64) NOT NULL,
    quantity_required NUMERIC(19, 4) NOT NULL CHECK (quantity_required > 0),
    unit VARCHAR(32) NOT NULL,
    quantity_reserved NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (quantity_reserved >= 0),
    quantity_consumed NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (quantity_consumed >= 0),
    quantity_short NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (quantity_short >= 0),
    UNIQUE (work_order_id, component_product_id)
);
CREATE INDEX IF NOT EXISTS idx_mfg_work_order_requirements_order_id
    ON mfg_work_order_requirements (work_order_id);

CREATE TABLE IF NOT EXISTS mfg_scrap_reasons (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code VARCHAR(64) NOT NULL UNIQUE,
    label VARCHAR(120) NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS mfg_units (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code VARCHAR(32) NOT NULL UNIQUE,
    label VARCHAR(120) NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS mfg_production_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id UUID NOT NULL REFERENCES mfg_work_orders (id),
    quantity_produced NUMERIC(19, 4) NOT NULL CHECK (quantity_produced > 0),
    quantity_scrapped NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (quantity_scrapped >= 0),
    scrap_reason_id UUID REFERENCES mfg_scrap_reasons (id),
    work_center_id UUID REFERENCES mfg_work_centers (id),
    operator_id VARCHAR(64) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'completed'
        CHECK (status IN ('completed',)),
    notes TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (scrap_reason_id IS NOT NULL OR quantity_scrapped = 0)
);
CREATE INDEX IF NOT EXISTS idx_mfg_production_runs_work_order_id
    ON mfg_production_runs (work_order_id);
CREATE INDEX IF NOT EXISTS idx_mfg_production_runs_completed_at
    ON mfg_production_runs (completed_at);

CREATE TABLE IF NOT EXISTS mfg_reservations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id UUID NOT NULL REFERENCES mfg_work_orders (id),
    component_product_id VARCHAR(64) NOT NULL,
    quantity_reserved NUMERIC(19, 4) NOT NULL CHECK (quantity_reserved > 0),
    quantity_consumed NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (quantity_consumed >= 0),
    unit VARCHAR(32) NOT NULL,
    status VARCHAR(30) NOT NULL DEFAULT 'pending_confirmation'
        CHECK (status IN ('pending_confirmation', 'reserved', 'partially_consumed', 'consumed', 'released', 'short')),
    stock_unknown BOOLEAN NOT NULL DEFAULT TRUE,
    confirmation_reference VARCHAR(128),
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_mfg_reservations_work_order_id
    ON mfg_reservations (work_order_id);
CREATE INDEX IF NOT EXISTS idx_mfg_reservations_component_product_id
    ON mfg_reservations (component_product_id);
CREATE INDEX IF NOT EXISTS idx_mfg_reservations_status ON mfg_reservations (status);

CREATE TABLE IF NOT EXISTS mfg_stock_notices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind VARCHAR(20) NOT NULL CHECK (kind IN ('confirmed', 'short')),
    work_order_id UUID REFERENCES mfg_work_orders (id),
    reservation_id UUID REFERENCES mfg_reservations (id),
    component_product_id VARCHAR(64),
    quantity NUMERIC(19, 4),
    message TEXT,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_mfg_stock_notices_work_order_id
    ON mfg_stock_notices (work_order_id);
