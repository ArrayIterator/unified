-- Field Service / Equipment Maintenance module tables for the
-- Unified Pentagonal Project (`src/modules/field_service.rs`):
-- equipment registry, maintenance plans, work orders, checklist
-- items, part lines, dispatch assignments, and customer
-- sign-offs. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the gamification precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `asset_id`, `stock_id`, `project_id`, and `technician_id` /
--     `assignee_id` / `created_by` columns are plain references
--     with NO foreign key on purpose: assets, stock, projects, and
--     users live in other modules and this module must not assume
--     their table or migration order. `company_id` is not stored
--     here at all.
--   - `field_equipment.warranty_end` is a local copy of the
--     warranty date owned by the assets module; it is refreshed
--     from `assets.updated` events.
--   - `field_work_order_parts.status` is one of `required`,
--     `reserved`, `consumption_requested`, or `returned`.
--     `consumption_requested` means a stock adjustment was
--     requested from the assets module; stock levels are never
--     decremented by this module.
--   - `field_signoffs.signature_ref` is a storage reference only
--     (opaque key handed to the storage module); no signature
--     bytes are stored here.
--   - `field_signoffs.follow_up_status` is `none`, `requested`,
--     or `pending_no_distribution`.
--   - All ids are Uuid, all timestamps are Timestamptz. JSON
--     columns are JSONB.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Equipment registry.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS field_equipment (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_id UUID,
    equipment_type_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    serial_number TEXT CHECK (serial_number IS NULL OR char_length(serial_number) BETWEEN 1 AND 128),
    location TEXT CHECK (location IS NULL OR char_length(location) BETWEEN 1 AND 200),
    install_date DATE,
    warranty_end DATE,
    status TEXT NOT NULL CHECK (char_length(status) BETWEEN 1 AND 64),
    usage_reading DOUBLE PRECISION CHECK (usage_reading IS NULL OR usage_reading >= 0),
    usage_unit TEXT CHECK (usage_unit IS NULL OR char_length(usage_unit) BETWEEN 1 AND 32),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_field_equipment_type ON field_equipment (equipment_type_id);
CREATE INDEX IF NOT EXISTS idx_field_equipment_asset ON field_equipment (asset_id);
CREATE INDEX IF NOT EXISTS idx_field_equipment_status ON field_equipment (status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_field_equipment_serial ON field_equipment (serial_number) WHERE serial_number IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Recurring maintenance plans.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS field_maintenance_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    equipment_id UUID NOT NULL,
    template_id UUID,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    interval_value BIGINT NOT NULL CHECK (interval_value BETWEEN 1 AND 3650),
    interval_unit TEXT NOT NULL CHECK (char_length(interval_unit) BETWEEN 1 AND 64),
    last_service_at TIMESTAMPTZ,
    usage_at_last_service DOUBLE PRECISION CHECK (usage_at_last_service IS NULL OR usage_at_last_service >= 0),
    next_due_at TIMESTAMPTZ,
    next_due_usage DOUBLE PRECISION CHECK (next_due_usage IS NULL OR next_due_usage >= 0),
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_field_plans_equipment ON field_maintenance_plans (equipment_id);
CREATE INDEX IF NOT EXISTS idx_field_plans_next_due ON field_maintenance_plans (next_due_at) WHERE is_active;
CREATE INDEX IF NOT EXISTS idx_field_plans_template ON field_maintenance_plans (template_id);

-- ---------------------------------------------------------------------------
-- Work orders.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS field_work_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    equipment_id UUID NOT NULL,
    plan_id UUID,
    project_id TEXT CHECK (project_id IS NULL OR char_length(project_id) BETWEEN 1 AND 200),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    order_type TEXT NOT NULL CHECK (char_length(order_type) BETWEEN 1 AND 64),
    priority TEXT NOT NULL CHECK (char_length(priority) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('open', 'assigned', 'in_progress', 'on_hold', 'completed', 'verified', 'closed', 'cancelled')),
    assignee_id TEXT CHECK (assignee_id IS NULL OR char_length(assignee_id) BETWEEN 1 AND 200),
    scheduled_at TIMESTAMPTZ,
    due_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    verified_at TIMESTAMPTZ,
    closed_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (due_at IS NULL OR scheduled_at IS NULL OR due_at >= scheduled_at)
);
CREATE INDEX IF NOT EXISTS idx_field_work_orders_equipment ON field_work_orders (equipment_id);
CREATE INDEX IF NOT EXISTS idx_field_work_orders_plan ON field_work_orders (plan_id);
CREATE INDEX IF NOT EXISTS idx_field_work_orders_project ON field_work_orders (project_id);
CREATE INDEX IF NOT EXISTS idx_field_work_orders_status ON field_work_orders (status);
CREATE INDEX IF NOT EXISTS idx_field_work_orders_assignee ON field_work_orders (assignee_id);
CREATE INDEX IF NOT EXISTS idx_field_work_orders_due ON field_work_orders (due_at);

-- ---------------------------------------------------------------------------
-- Work order checklist items.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS field_work_order_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id UUID NOT NULL,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    position BIGINT NOT NULL DEFAULT 0 CHECK (position BETWEEN 0 AND 10000),
    is_done BOOLEAN NOT NULL DEFAULT FALSE,
    done_by TEXT CHECK (done_by IS NULL OR char_length(done_by) BETWEEN 1 AND 200),
    done_at TIMESTAMPTZ,
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_field_work_order_items_order ON field_work_order_items (work_order_id, position);

-- ---------------------------------------------------------------------------
-- Work order part lines.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS field_work_order_parts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id UUID NOT NULL,
    stock_id UUID,
    part_name TEXT NOT NULL CHECK (char_length(part_name) BETWEEN 1 AND 200),
    quantity BIGINT NOT NULL CHECK (quantity BETWEEN 1 AND 100000),
    unit TEXT NOT NULL DEFAULT 'unit' CHECK (char_length(unit) BETWEEN 1 AND 32),
    status TEXT NOT NULL DEFAULT 'required' CHECK (status IN ('required', 'reserved', 'consumption_requested', 'returned')),
    consumed_at TIMESTAMPTZ,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_field_work_order_parts_order ON field_work_order_parts (work_order_id);
CREATE INDEX IF NOT EXISTS idx_field_work_order_parts_stock ON field_work_order_parts (stock_id);

-- ---------------------------------------------------------------------------
-- Dispatch assignments.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS field_dispatch_assignments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id UUID NOT NULL,
    technician_id TEXT NOT NULL CHECK (char_length(technician_id) BETWEEN 1 AND 200),
    sequence BIGINT NOT NULL DEFAULT 1 CHECK (sequence BETWEEN 0 AND 10000),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'completed', 'released')),
    assigned_by TEXT NOT NULL CHECK (char_length(assigned_by) BETWEEN 1 AND 200),
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    released_at TIMESTAMPTZ,
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_field_dispatch_technician ON field_dispatch_assignments (technician_id, status);
CREATE INDEX IF NOT EXISTS idx_field_dispatch_order ON field_dispatch_assignments (work_order_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_field_dispatch_active_order ON field_dispatch_assignments (work_order_id) WHERE status = 'active';

-- ---------------------------------------------------------------------------
-- Customer sign-offs.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS field_signoffs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id UUID NOT NULL,
    customer_name TEXT NOT NULL CHECK (char_length(customer_name) BETWEEN 1 AND 200),
    signature_ref TEXT NOT NULL CHECK (char_length(signature_ref) BETWEEN 1 AND 200),
    signed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    survey_distribution_id UUID,
    follow_up_status TEXT NOT NULL DEFAULT 'none' CHECK (follow_up_status IN ('none', 'requested', 'pending_no_distribution')),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_field_signoffs_order ON field_signoffs (work_order_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_field_signoffs_one_per_order ON field_signoffs (work_order_id);
