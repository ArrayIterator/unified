-- Procurement / Vendor Management tables for the Unified Pentagonal
-- Project (`src/modules/procurement.rs`): vendors, purchase orders
-- with lines and an approval workflow, goods receipts, and spend
-- budgets.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/procurement.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `vendors.payment_reference` is a reference only (for example
--     "bank transfer — details on file with finance"): raw bank
--     account details are never stored here. `vendors.rating` is a
--     manual quality rating, 1..=5, when set. `vendors.status` is
--     `active`, `inactive`, or `suspended` (`VendorStatus`);
--     suspended vendors cannot receive new purchase orders.
--   - `purchase_orders.po_number` is unique (`PO-YYYY-NNNNNN` in
--     the module). `status` is `draft`, `pending_approval`,
--     `approved`, `ordered`, `partially_received`, `received`,
--     `closed`, or `cancelled` (`PurchaseOrderStatus`). A rejection
--     returns the PO to `draft` so it can be corrected and
--     resubmitted; the rejection row stays on the record.
--   - Money is NUMERIC(19,4): `purchase_orders.subtotal` /
--     `tax_total` / `total` and `purchase_order_lines.unit_price`
--     / `line_total` / `quantity` / `received_quantity`. Totals
--     are always computed server-side from the lines and are never
--     accepted from the client. `tax_rate` is a fraction (0.10 =
--     10%). The CHECK on `received_quantity <= quantity` is the
--     second line of defence behind the module's over-receipt
--     rejection.
--   - `purchase_order_approvals` records every approval and
--     rejection: approver id, approver role, decision
--     (`approved` / `rejected`), and reason (required for
--     rejections).
--   - `goods_receipts` record quantities received against one PO
--     line, with an optional quality note. Receipts record
--     delivery; they do not post inventory stock.
--   - `procurement_budgets` scope to a category, a vendor, or
--     both (at least one scope is required). Spend is computed on
--     read from committed purchase orders (approved and later,
--     never draft / pending approval / cancelled) inside the
--     period; an over-budget state warns, it never blocks.
--   - `purchase_orders.vendor_id` is RESTRICT on delete (a vendor
--     with purchase orders cannot be deleted); `requested_by`,
--     `approved_by`, `approver_id`, and `received_by` are plain
--     VARCHAR user ids, exactly like `store_orders.user_id` in
--     migration 000002. No foreign key to `users` is stored here.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One vendor.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS vendors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    contact_name VARCHAR(200),
    email VARCHAR(254),
    phone VARCHAR(64),
    address VARCHAR(500),
    categories TEXT[] NOT NULL DEFAULT '{}',
    tags TEXT[] NOT NULL DEFAULT '{}',
    status VARCHAR(20) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'inactive', 'suspended')),
    payment_reference VARCHAR(200),
    notes TEXT,
    rating SMALLINT CHECK (rating IS NULL OR (rating BETWEEN 1 AND 5)),
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_vendors_status ON vendors (status);
CREATE INDEX IF NOT EXISTS idx_vendors_name ON vendors (name);

-- ---------------------------------------------------------------------------
-- One purchase order.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS purchase_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    po_number VARCHAR(32) NOT NULL UNIQUE,
    vendor_id UUID NOT NULL REFERENCES vendors (id) ON DELETE RESTRICT,
    title VARCHAR(200) NOT NULL,
    category VARCHAR(80),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR(20) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'pending_approval', 'approved', 'ordered', 'partially_received', 'received', 'closed', 'cancelled')),
    subtotal NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (subtotal >= 0),
    tax_total NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (tax_total >= 0),
    total NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (total >= 0),
    promised_date DATE,
    ordered_at TIMESTAMPTZ,
    received_at TIMESTAMPTZ,
    cancel_reason TEXT,
    requested_by VARCHAR(255) NOT NULL,
    approved_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_purchase_orders_vendor_id ON purchase_orders (vendor_id);
CREATE INDEX IF NOT EXISTS idx_purchase_orders_status ON purchase_orders (status);
CREATE INDEX IF NOT EXISTS idx_purchase_orders_category ON purchase_orders (category);
CREATE INDEX IF NOT EXISTS idx_purchase_orders_po_number ON purchase_orders (po_number);

-- ---------------------------------------------------------------------------
-- One purchase order line.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS purchase_order_lines (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    purchase_order_id UUID NOT NULL REFERENCES purchase_orders (id) ON DELETE CASCADE,
    description TEXT NOT NULL,
    quantity NUMERIC(19,4) NOT NULL CHECK (quantity > 0),
    unit_price NUMERIC(19,4) NOT NULL CHECK (unit_price >= 0),
    tax_rate NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (tax_rate >= 0 AND tax_rate <= 1),
    line_total NUMERIC(19,4) NOT NULL CHECK (line_total >= 0),
    received_quantity NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (received_quantity >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (received_quantity <= quantity)
);
CREATE INDEX IF NOT EXISTS idx_purchase_order_lines_po_id ON purchase_order_lines (purchase_order_id);

-- ---------------------------------------------------------------------------
-- One approval workflow row for a purchase order.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS purchase_order_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    purchase_order_id UUID NOT NULL REFERENCES purchase_orders (id) ON DELETE CASCADE,
    approver_id VARCHAR(255) NOT NULL,
    approver_role VARCHAR(100) NOT NULL,
    decision VARCHAR(20) NOT NULL CHECK (decision IN ('approved', 'rejected')),
    reason TEXT,
    decided_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_purchase_order_approvals_po_id ON purchase_order_approvals (purchase_order_id);
CREATE INDEX IF NOT EXISTS idx_purchase_order_approvals_decision ON purchase_order_approvals (decision);

-- ---------------------------------------------------------------------------
-- One goods receipt against a purchase order line.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS goods_receipts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    purchase_order_id UUID NOT NULL REFERENCES purchase_orders (id) ON DELETE CASCADE,
    line_id UUID NOT NULL REFERENCES purchase_order_lines (id) ON DELETE CASCADE,
    quantity_received NUMERIC(19,4) NOT NULL CHECK (quantity_received > 0),
    quality_note TEXT,
    received_by VARCHAR(255) NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_goods_receipts_po_id ON goods_receipts (purchase_order_id);
CREATE INDEX IF NOT EXISTS idx_goods_receipts_line_id ON goods_receipts (line_id);

-- ---------------------------------------------------------------------------
-- One spend budget, scoped to a category, a vendor, or both.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS procurement_budgets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category VARCHAR(80),
    vendor_id UUID REFERENCES vendors (id) ON DELETE CASCADE,
    period_start DATE NOT NULL,
    period_end DATE NOT NULL,
    budget_amount NUMERIC(19,4) NOT NULL CHECK (budget_amount >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (period_end >= period_start),
    CHECK (category IS NOT NULL OR vendor_id IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS idx_procurement_budgets_category ON procurement_budgets (category);
CREATE INDEX IF NOT EXISTS idx_procurement_budgets_vendor_id ON procurement_budgets (vendor_id);
CREATE INDEX IF NOT EXISTS idx_procurement_budgets_period ON procurement_budgets (period_start, period_end);
