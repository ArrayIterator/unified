-- Rental module (`src/modules/rental/`): rentable products with
-- data-driven per-period pricing, orders with a status machine,
-- hand-out / return deliveries, damage reports, and deposit
-- handling. PostgreSQL only. Run with diesel_migrations
-- (embedded via crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000110_forums: the
-- in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz. `user_id`
--     / `customer_id` / `reported_by` / `resolved_by` /
--     `recorded_by` are plain TEXT with NO foreign key on purpose,
--     matching the forums convention: users live in the users
--     module and this module must not assume its table or
--     migration order. `product_id`, `contract_id`, and
--     `shipment_id` are likewise references by id only, with NO
--     foreign key: products live in the store/products modules,
--     agreements in the contracts module, and shipments in the
--     logistics module.
--   - `tenant_id` is plain TEXT on every row: rental data is
--     tenant business data and every query filters by it. A row
--     from another tenant reads as 404, never 403.
--   - Money is integer minor units everywhere: `price_minor`,
--     `deposit_amount_minor`, `rental_amount_minor`,
--     `late_fee_minor`, `damage_total_minor`, `total_minor`,
--     `deposit_deducted_minor`, `estimated_cost_minor`, and the
--     late-fee rule amounts. There is no floating point money in
--     this module.
--   - Periods and late-fee rules are data, not code: a rate
--     names its own `period_unit` (`hour`, `day`, `week`,
--     `month`) plus `period_length`, and the late-fee rule on a
--     listing names its own `basis` (`per_day`, `per_hour`,
--     `flat`) plus amount, grace hours, and an optional cap. A
--     month is 30 days by definition for pricing.
--   - Unique indexes enforce the module invariants in the
--     database too: one slug per (tenant, listing); one rate
--     label per listing. Availability (no double-booking) is
--     enforced by the module's overlap check, not by a database
--     constraint, because overlapping ranges cannot be expressed
--     as a unique index.
--   - `rental_orders.status` is `draft`, `reserved`, `out`,
--     `returned`, `completed`, or `cancelled`;
--     `deposit_status` is `pending`, `held`, `refunded`,
--     `deducted`, or `partially_refunded`;
--     `rental_deliveries.kind` is `handover` or `return`;
--     `rental_damage_reports.status` is `open`, `resolved`, or
--     `dismissed`. `photo_ids` references documents/storage
--     objects by id only (metadata); this module never stores
--     file bytes.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Listings: a store product made rentable, with a deposit and a
-- late-fee rule. `quantity` is how many identical units of this
-- listing can be rented at once.
CREATE TABLE IF NOT EXISTS rental_listings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    product_id TEXT NOT NULL CHECK (char_length(product_id) BETWEEN 1 AND 200),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 128),
    currency TEXT NOT NULL CHECK (char_length(currency) = 3),
    deposit_amount_minor BIGINT NOT NULL DEFAULT 0 CHECK (deposit_amount_minor >= 0),
    quantity BIGINT NOT NULL DEFAULT 1 CHECK (quantity > 0),
    late_fee_basis TEXT NOT NULL DEFAULT 'per_day' CHECK (late_fee_basis IN ('per_day', 'per_hour', 'flat')),
    late_fee_amount_minor BIGINT NOT NULL DEFAULT 0 CHECK (late_fee_amount_minor >= 0),
    late_fee_grace_hours BIGINT NOT NULL DEFAULT 0 CHECK (late_fee_grace_hours >= 0),
    late_fee_cap_minor BIGINT CHECK (late_fee_cap_minor IS NULL OR late_fee_cap_minor >= 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_rental_listings_tenant_slug ON rental_listings (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_rental_listings_tenant_product ON rental_listings (tenant_id, product_id);
CREATE INDEX IF NOT EXISTS idx_rental_listings_tenant_active ON rental_listings (tenant_id, active);

-- Rates: one per-period price of a listing. `period_unit` plus
-- `period_length` define the period; `period_label` is the
-- display name shown to renters.
CREATE TABLE IF NOT EXISTS rental_rates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    listing_id UUID NOT NULL,
    period_label TEXT NOT NULL CHECK (char_length(period_label) BETWEEN 1 AND 64),
    period_unit TEXT NOT NULL CHECK (period_unit IN ('hour', 'day', 'week', 'month')),
    period_length BIGINT NOT NULL CHECK (period_length > 0),
    price_minor BIGINT NOT NULL CHECK (price_minor >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_rental_rates_listing_label ON rental_rates (listing_id, period_label);
CREATE INDEX IF NOT EXISTS idx_rental_rates_listing ON rental_rates (listing_id, period_unit);

-- Orders: one rental contract. `rental_amount_minor` is priced
-- at booking time from the chosen rate; `late_fee_minor` is
-- computed at return; `damage_total_minor` grows when damage
-- reports resolve; `total_minor` is rental + late fee + damage.
-- `contract_id` references a contracts-module agreement by id
-- only; `shipment_id` is set by the logistics delivery
-- listener.
CREATE TABLE IF NOT EXISTS rental_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    listing_id UUID NOT NULL,
    rate_id UUID NOT NULL,
    customer_id TEXT NOT NULL CHECK (char_length(customer_id) BETWEEN 1 AND 200),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'reserved', 'out', 'returned', 'completed', 'cancelled')),
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    quantity BIGINT NOT NULL CHECK (quantity > 0),
    currency TEXT NOT NULL CHECK (char_length(currency) = 3),
    rental_amount_minor BIGINT NOT NULL CHECK (rental_amount_minor >= 0),
    deposit_amount_minor BIGINT NOT NULL DEFAULT 0 CHECK (deposit_amount_minor >= 0),
    deposit_status TEXT NOT NULL DEFAULT 'pending' CHECK (deposit_status IN ('pending', 'held', 'refunded', 'deducted', 'partially_refunded')),
    deposit_deducted_minor BIGINT NOT NULL DEFAULT 0 CHECK (deposit_deducted_minor >= 0),
    late_fee_minor BIGINT NOT NULL DEFAULT 0 CHECK (late_fee_minor >= 0),
    damage_total_minor BIGINT NOT NULL DEFAULT 0 CHECK (damage_total_minor >= 0),
    total_minor BIGINT NOT NULL CHECK (total_minor >= 0),
    contract_id TEXT,
    agreement_signed BOOLEAN NOT NULL DEFAULT FALSE,
    shipment_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_rental_orders_range CHECK (ends_at > starts_at),
    CONSTRAINT chk_rental_orders_deducted_within_deposit CHECK (deposit_deducted_minor <= deposit_amount_minor)
);
CREATE INDEX IF NOT EXISTS idx_rental_orders_listing ON rental_orders (listing_id, starts_at);
CREATE INDEX IF NOT EXISTS idx_rental_orders_customer ON rental_orders (tenant_id, customer_id, starts_at);
CREATE INDEX IF NOT EXISTS idx_rental_orders_status ON rental_orders (tenant_id, status, starts_at);
CREATE INDEX IF NOT EXISTS idx_rental_orders_contract ON rental_orders (contract_id) WHERE contract_id IS NOT NULL;

-- Deliveries: one hand-out or return event per row, with
-- condition notes and photo metadata.
CREATE TABLE IF NOT EXISTS rental_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    order_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('handover', 'return')),
    condition_notes TEXT CHECK (condition_notes IS NULL OR char_length(condition_notes) BETWEEN 1 AND 5000),
    photo_ids TEXT[] NOT NULL DEFAULT '{}',
    recorded_by TEXT NOT NULL CHECK (char_length(recorded_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_rental_deliveries_order ON rental_deliveries (order_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_rental_deliveries_kind ON rental_deliveries (order_id, kind);

-- Damage reports: description plus estimated cost, resolved once
-- (resolving adds the cost to the order total) or dismissed.
CREATE TABLE IF NOT EXISTS rental_damage_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    order_id UUID NOT NULL,
    delivery_id UUID,
    description TEXT NOT NULL CHECK (char_length(description) BETWEEN 1 AND 5000),
    estimated_cost_minor BIGINT NOT NULL DEFAULT 0 CHECK (estimated_cost_minor >= 0),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved', 'dismissed')),
    reported_by TEXT NOT NULL CHECK (char_length(reported_by) BETWEEN 1 AND 200),
    resolved_by TEXT,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_rental_damage_resolved_fields CHECK ((status <> 'resolved' OR (resolved_by IS NOT NULL AND resolved_at IS NOT NULL)) AND (status <> 'dismissed' OR resolved_at IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS idx_rental_damage_reports_order ON rental_damage_reports (order_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_rental_damage_reports_status ON rental_damage_reports (tenant_id, status, created_at ASC);

-- Per-tenant rental settings (late-fee grace, advance booking
-- window, cancel cutoff).
CREATE TABLE IF NOT EXISTS rental_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    late_fee_grace_hours BIGINT NOT NULL DEFAULT 0 CHECK (late_fee_grace_hours >= 0),
    max_advance_booking_days BIGINT NOT NULL DEFAULT 365 CHECK (max_advance_booking_days > 0),
    cancel_cutoff_hours BIGINT NOT NULL DEFAULT 24 CHECK (cancel_cutoff_hours >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
