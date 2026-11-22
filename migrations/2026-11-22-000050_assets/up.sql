-- Asset & Inventory tables for the Unified Pentagonal Project
-- (`src/modules/assets.rs`): hardware / software inventory,
-- assignment history, consumable stock levels, audited stock
-- adjustments, and stock-take audits.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/assets.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `assets.created_by` and `asset_assignments.user_id` /
--     `assigned_by` are plain VARCHAR user ids: the module
--     identifies callers by the `X-User-Id` header and keeps user
--     ids as strings, exactly like `store_orders.user_id` in
--     migration 000002. No foreign key to `users` is stored here.
--   - `assets.vendor` is a plain VARCHAR reference to the vendor
--     record kept on the procurement side; no procurement table is
--     referenced here. `assets.serial_number` is unique when
--     present (NULL values do not collide).
--   - `assets.status` is `in_stock`, `assigned`, `in_repair`,
--     `retired`, or `disposed` (`AssetStatus`); `asset_type` is
--     `hardware`, `software`, `consumable`, `furniture`, `vehicle`,
--     or `other` (`AssetType`). Money (`purchase_price`,
--     `salvage_value`) is NUMERIC(19,4), never negative, and
--     `salvage_value` never exceeds `purchase_price`.
--   - `assets.depreciation_status` is `none`, `pending`, or
--     `computed`. The depreciation schedule itself is JSONB: the
--     math lives in the Calculator module, this module only stores
--     the schedule the `calculator.depreciation_requested` event
--     returns. `depreciation_method` is `straight_line` or
--     `declining_balance`; `useful_life_years` is 1..100.
--   - `asset_assignments` is append-only history: a return sets
--     `returned_at` (and `return_reason`) on the active row; rows
--     are never deleted. At most one `active` assignment per asset
--     is enforced by the partial unique index below.
--   - `asset_stock` holds one row per consumable asset
--     (enforced by the unique index on `asset_id`); quantities
--     are never negative and `reorder_threshold` is never
--     negative. Every change goes through
--     `asset_stock_adjustments`, which always carries a reason.
--   - `asset_stock_takes.status` is `open` or `closed`;
--     `asset_stock_take_lines.counted_quantity` is NULL until the
--     line is counted, and `discrepancy` is derived
--     (`counted - expected`) in the module, never stored.

CREATE TABLE IF NOT EXISTS assets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    asset_type VARCHAR(20) NOT NULL DEFAULT 'other'
        CHECK (asset_type IN ('hardware', 'software', 'consumable', 'furniture', 'vehicle', 'other')),
    serial_number VARCHAR(128) UNIQUE,
    purchase_date DATE,
    purchase_price NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (purchase_price >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    vendor VARCHAR(200),
    location VARCHAR(200),
    status VARCHAR(20) NOT NULL DEFAULT 'in_stock'
        CHECK (status IN ('in_stock', 'assigned', 'in_repair', 'retired', 'disposed')),
    warranty_end DATE,
    license_expires_at DATE,
    license_seats BIGINT CHECK (license_seats IS NULL OR license_seats >= 0),
    depreciation_method VARCHAR(20)
        CHECK (depreciation_method IS NULL OR depreciation_method IN ('straight_line', 'declining_balance')),
    useful_life_years BIGINT
        CHECK (useful_life_years IS NULL OR (useful_life_years >= 1 AND useful_life_years <= 100)),
    salvage_value NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (salvage_value >= 0),
    depreciation_status VARCHAR(20) NOT NULL DEFAULT 'none'
        CHECK (depreciation_status IN ('none', 'pending', 'computed')),
    depreciation_schedule JSONB,
    depreciation_error TEXT,
    created_by VARCHAR(200) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (salvage_value <= purchase_price),
    CHECK (warranty_end IS NULL OR purchase_date IS NULL OR warranty_end >= purchase_date)
);

CREATE INDEX IF NOT EXISTS idx_assets_status ON assets (status);
CREATE INDEX IF NOT EXISTS idx_assets_asset_type ON assets (asset_type);
CREATE INDEX IF NOT EXISTS idx_assets_location ON assets (location);
CREATE INDEX IF NOT EXISTS idx_assets_warranty_end ON assets (warranty_end);
CREATE INDEX IF NOT EXISTS idx_assets_license_expires_at ON assets (license_expires_at);
CREATE INDEX IF NOT EXISTS idx_assets_created_by ON assets (created_by);

CREATE TABLE IF NOT EXISTS asset_assignments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_id UUID NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    user_id VARCHAR(200) NOT NULL,
    assigned_by VARCHAR(200) NOT NULL,
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    returned_at TIMESTAMPTZ,
    return_reason TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'returned')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (status <> 'returned' OR returned_at IS NOT NULL),
    CHECK (status <> 'active' OR returned_at IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_asset_assignments_asset_id ON asset_assignments (asset_id);
CREATE INDEX IF NOT EXISTS idx_asset_assignments_user_id ON asset_assignments (user_id);
CREATE INDEX IF NOT EXISTS idx_asset_assignments_status ON asset_assignments (status);
-- At most one active assignment per asset: assignment history is
-- append-only, and a return closes the active row.
CREATE UNIQUE INDEX IF NOT EXISTS uq_asset_assignments_one_active
    ON asset_assignments (asset_id) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS asset_stock (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_id UUID NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    quantity_on_hand BIGINT NOT NULL DEFAULT 0 CHECK (quantity_on_hand >= 0),
    reorder_threshold BIGINT NOT NULL DEFAULT 0 CHECK (reorder_threshold >= 0),
    location VARCHAR(200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One stock row per consumable asset.
CREATE UNIQUE INDEX IF NOT EXISTS uq_asset_stock_asset_id ON asset_stock (asset_id);
CREATE INDEX IF NOT EXISTS idx_asset_stock_quantity ON asset_stock (quantity_on_hand);
CREATE INDEX IF NOT EXISTS idx_asset_stock_low
    ON asset_stock (asset_id) WHERE quantity_on_hand <= reorder_threshold;

CREATE TABLE IF NOT EXISTS asset_stock_adjustments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stock_id UUID NOT NULL REFERENCES asset_stock (id) ON DELETE CASCADE,
    asset_id UUID NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    delta BIGINT NOT NULL CHECK (delta <> 0),
    quantity_after BIGINT NOT NULL CHECK (quantity_after >= 0),
    reason TEXT NOT NULL,
    adjusted_by VARCHAR(200) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_asset_stock_adjustments_stock_id
    ON asset_stock_adjustments (stock_id);
CREATE INDEX IF NOT EXISTS idx_asset_stock_adjustments_asset_id
    ON asset_stock_adjustments (asset_id);
CREATE INDEX IF NOT EXISTS idx_asset_stock_adjustments_created_at
    ON asset_stock_adjustments (created_at);

CREATE TABLE IF NOT EXISTS asset_stock_takes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    notes TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'closed')),
    started_by VARCHAR(200) NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ,
    closed_by VARCHAR(200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (status <> 'closed' OR closed_at IS NOT NULL),
    CHECK (status <> 'open' OR closed_at IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_asset_stock_takes_status ON asset_stock_takes (status);
CREATE INDEX IF NOT EXISTS idx_asset_stock_takes_started_at ON asset_stock_takes (started_at);

CREATE TABLE IF NOT EXISTS asset_stock_take_lines (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stock_take_id UUID NOT NULL REFERENCES asset_stock_takes (id) ON DELETE CASCADE,
    asset_id UUID NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    stock_id UUID NOT NULL REFERENCES asset_stock (id) ON DELETE CASCADE,
    expected_quantity BIGINT NOT NULL CHECK (expected_quantity >= 0),
    counted_quantity BIGINT CHECK (counted_quantity IS NULL OR counted_quantity >= 0),
    counted_by VARCHAR(200),
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_asset_stock_take_lines_take_id
    ON asset_stock_take_lines (stock_take_id);
CREATE INDEX IF NOT EXISTS idx_asset_stock_take_lines_asset_id
    ON asset_stock_take_lines (asset_id);
-- One count line per asset per stock take.
CREATE UNIQUE INDEX IF NOT EXISTS uq_asset_stock_take_lines_take_asset
    ON asset_stock_take_lines (stock_take_id, asset_id);
