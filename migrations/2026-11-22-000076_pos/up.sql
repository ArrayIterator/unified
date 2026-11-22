-- Point of Sale tables for the Unified Pentagonal Project
-- (`src/modules/pos.rs`): registers, shifts, sales, sale lines,
-- payments, refunds, and receipts.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/pos.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `pos_registers.cashiers` is a TEXT array of user ids: the
--     module identifies callers by the `X-User-Id` header and keeps
--     user ids as strings, exactly like `store_orders.user_id` in
--     migration 000002. No foreign key to `users` is stored here.
--   - `pos_sales.cashier_id` is likewise a plain VARCHAR user id.
--     `pos_sales.number` (`POS-000001`) and
--     `pos_receipts.number` (`RCP-000001`) are unique, monotonic,
--     and never reused.
--   - Money is NUMERIC(19,4), never negative where a CHECK
--     applies; the module rounds to cents (2 decimals) in major
--     units. `pos_sales.status` is `draft`, `held`, `completed`,
--     `voided`, `partially_refunded`, or `refunded`;
--     `pos_shifts.status` is `open` or `closed`.
--   - `pos_sale_items.discount_type` is `percent` or `fixed`;
--     `pos_sale_items.tax_rate` is a fraction (0.10 means 10%).
--     `pos_payments.tendered` is the cash handed over and is
--     always at least `amount`; the change due is
--     `tendered - amount` summed across payments.
--   - `pos_payment_methods` and the POS settings are admin-editable
--     runtime configuration (same pattern as the billing settings);
--     their durable form is a follow-up for the settings store, so
--     no table is created for them here.
--   - `pos_receipts.print_lines` is JSONB: the structured print
--     lines a receipt printer renders in order. `pos_receipts`
--     holds one row per completed sale (unique on `sale_id`).
--   - `pos_refunds.items` is JSONB (the refunded lines with
--     quantities and amounts); `pos_refunds.method_id` is a plain
--     VARCHAR reference to the payment-method definition id, not a
--     foreign key, because method definitions live in runtime
--     configuration (see above).

CREATE TABLE IF NOT EXISTS pos_registers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    code VARCHAR(64) NOT NULL UNIQUE,
    location VARCHAR(200),
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'inactive')),
    cashiers TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS pos_shifts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    register_id UUID NOT NULL REFERENCES pos_registers (id),
    cashier_id VARCHAR(64) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'closed')),
    opening_float NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (opening_float >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    expected_cash NUMERIC(19, 4) CHECK (expected_cash IS NULL OR expected_cash >= 0),
    counted_cash NUMERIC(19, 4) CHECK (counted_cash IS NULL OR counted_cash >= 0),
    variance NUMERIC(19, 4),
    notes TEXT,
    opened_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_pos_shifts_register_id ON pos_shifts (register_id);
CREATE INDEX IF NOT EXISTS idx_pos_shifts_cashier_id ON pos_shifts (cashier_id);
CREATE INDEX IF NOT EXISTS idx_pos_shifts_status ON pos_shifts (status);

CREATE TABLE IF NOT EXISTS pos_sales (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    number VARCHAR(32) NOT NULL UNIQUE,
    register_id UUID NOT NULL REFERENCES pos_registers (id),
    shift_id UUID REFERENCES pos_shifts (id),
    cashier_id VARCHAR(64) NOT NULL,
    company_id VARCHAR(64),
    discount_type VARCHAR(20) CHECK (discount_type IS NULL OR discount_type IN ('percent', 'fixed')),
    discount_value NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (discount_value >= 0),
    discount_total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (discount_total >= 0),
    subtotal NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (subtotal >= 0),
    tax_total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (tax_total >= 0),
    total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (total >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    tax_inclusive BOOLEAN NOT NULL DEFAULT FALSE,
    status VARCHAR(30) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'held', 'completed', 'voided', 'partially_refunded', 'refunded')),
    amount_paid NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (amount_paid >= 0),
    change_due NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (change_due >= 0),
    refunded_amount NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (refunded_amount >= 0),
    customer_name VARCHAR(200),
    notes TEXT,
    completed_at TIMESTAMPTZ,
    voided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_pos_sales_register_id ON pos_sales (register_id);
CREATE INDEX IF NOT EXISTS idx_pos_sales_shift_id ON pos_sales (shift_id);
CREATE INDEX IF NOT EXISTS idx_pos_sales_cashier_id ON pos_sales (cashier_id);
CREATE INDEX IF NOT EXISTS idx_pos_sales_status ON pos_sales (status);
CREATE INDEX IF NOT EXISTS idx_pos_sales_completed_at ON pos_sales (completed_at);

CREATE TABLE IF NOT EXISTS pos_sale_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sale_id UUID NOT NULL REFERENCES pos_sales (id) ON DELETE CASCADE,
    product_id VARCHAR(64) NOT NULL,
    name VARCHAR(200) NOT NULL,
    barcode VARCHAR(64),
    sku VARCHAR(64),
    unit_price NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (unit_price >= 0),
    quantity INTEGER NOT NULL DEFAULT 1 CHECK (quantity >= 1),
    refunded_quantity INTEGER NOT NULL DEFAULT 0 CHECK (refunded_quantity >= 0),
    discount_type VARCHAR(20) CHECK (discount_type IS NULL OR discount_type IN ('percent', 'fixed')),
    discount_value NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (discount_value >= 0),
    discount_amount NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (discount_amount >= 0),
    line_subtotal NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (line_subtotal >= 0),
    tax_rate NUMERIC(10, 6) NOT NULL DEFAULT 0 CHECK (tax_rate >= 0),
    tax_amount NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (tax_amount >= 0),
    line_total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (line_total >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_pos_sale_items_sale_id ON pos_sale_items (sale_id);
CREATE INDEX IF NOT EXISTS idx_pos_sale_items_product_id ON pos_sale_items (product_id);

CREATE TABLE IF NOT EXISTS pos_payments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sale_id UUID NOT NULL REFERENCES pos_sales (id) ON DELETE CASCADE,
    method_id VARCHAR(64) NOT NULL,
    method_code VARCHAR(64) NOT NULL,
    amount NUMERIC(19, 4) NOT NULL CHECK (amount > 0),
    tendered NUMERIC(19, 4) NOT NULL CHECK (tendered >= amount),
    reference VARCHAR(64),
    recorded_by VARCHAR(64) NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_pos_payments_sale_id ON pos_payments (sale_id);
CREATE INDEX IF NOT EXISTS idx_pos_payments_method_id ON pos_payments (method_id);

CREATE TABLE IF NOT EXISTS pos_refunds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sale_id UUID NOT NULL REFERENCES pos_sales (id) ON DELETE CASCADE,
    shift_id UUID REFERENCES pos_shifts (id),
    amount NUMERIC(19, 4) NOT NULL CHECK (amount > 0),
    method_id VARCHAR(64) NOT NULL,
    method_code VARCHAR(64) NOT NULL,
    items JSONB NOT NULL DEFAULT '[]'::jsonb,
    reason TEXT NOT NULL,
    refunded_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_pos_refunds_sale_id ON pos_refunds (sale_id);
CREATE INDEX IF NOT EXISTS idx_pos_refunds_shift_id ON pos_refunds (shift_id);
CREATE INDEX IF NOT EXISTS idx_pos_refunds_created_at ON pos_refunds (created_at);

CREATE TABLE IF NOT EXISTS pos_receipts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    number VARCHAR(32) NOT NULL UNIQUE,
    sale_id UUID NOT NULL UNIQUE REFERENCES pos_sales (id) ON DELETE CASCADE,
    sale_number VARCHAR(32) NOT NULL,
    register_id UUID NOT NULL REFERENCES pos_registers (id),
    register_code VARCHAR(64),
    cashier_id VARCHAR(64) NOT NULL,
    store_name VARCHAR(200) NOT NULL,
    store_address VARCHAR(200),
    subtotal NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (subtotal >= 0),
    discount_total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (discount_total >= 0),
    tax_total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (tax_total >= 0),
    total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (total >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    tax_breakdown JSONB NOT NULL DEFAULT '[]'::jsonb,
    amount_paid NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (amount_paid >= 0),
    change_due NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (change_due >= 0),
    print_lines JSONB NOT NULL DEFAULT '[]'::jsonb,
    footer TEXT,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_pos_receipts_register_id ON pos_receipts (register_id);
CREATE INDEX IF NOT EXISTS idx_pos_receipts_cashier_id ON pos_receipts (cashier_id);
CREATE INDEX IF NOT EXISTS idx_pos_receipts_issued_at ON pos_receipts (issued_at);
