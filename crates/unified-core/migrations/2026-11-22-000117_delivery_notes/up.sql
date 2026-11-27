-- Delivery notes module (`src/modules/delivery_notes/`):
-- delivery notes generated from logistics
-- shipments, with the shipment's items, sender, receiver,
-- and addresses snapshotted at creation time, data-driven DO
-- numbering (prefix, width, reset rule per tenant), a pure
-- status machine (draft, issued, in transit, delivered,
-- returned, cancelled) with proof-of-delivery gating on
-- delivery, partial and full returns with per-item returned
-- quantities and rental-order linkage, and print-ready
-- documents queued through the printing module.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000116_phone: the
-- in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz.
--     `shipment_id` / `warehouse_id` / `driver_id` /
--     `vehicle_id` / `company_id` / `proof_of_delivery_id`
--     reference logistics / company rows by id with NO foreign
--     key, so deleting one of those rows never orphans a
--     delivery note, and this module must not assume their
--     tables or migration order. `created_by` is plain TEXT
--     with NO foreign key on purpose, matching the phone
--     convention: users live in the users module and this
--     module must not assume its table or migration order.
--     `order_id` / `rental_order_id` / `product_id` are plain
--     TEXT references to store / rental / product rows, also
--     with NO foreign key.
--   - `tenant_id` is plain TEXT on every row: delivery notes
--     are tenant business data and every query filters by it.
--     A row from another tenant reads as 404, never 403.
--   - Nothing is hard-coded: the DO number format (prefix,
--     width, reset rule) and the print defaults live in
--     `delivery_note_settings`, one row per tenant; templates
--     are referenced by id only. The sequence that feeds the
--     number lives in the module's in-memory store; the unique
--     index on (`tenant_id`, `number`) below is the durable
--     guard against two DOs sharing a number.
--   - There is no floating point money in this module:
--     delivery notes carry no money fields. Quantities are
--     whole units (BIGINT); `quantity_ordered` and
--     `quantity_shipped` are frozen at creation, and only
--     `quantity_returned` grows, through the return endpoint.
--   - `delivery_notes.status` is `draft`, `issued`,
--     `in_transit`, `delivered`, `returned`, or `cancelled`;
--     `delivery_notes.shipment_link_status` is `linked`,
--     `unverified`, or `none`; `delivery_notes.return_scope`
--     is `full`, `partial`, or `none`.
--   - The sender, receiver, driver, vehicle, and warehouse are
--     snapshots taken at creation time: later edits to a
--     shipment, order, or company record never rewrite a
--     delivery note's history, so the snapshot columns are
--     plain text on the note row, not references.
--   - Delivery proof is a logistics `ProofOfDelivery` id plus
--     the recipient signature metadata stored on the note
--     (`recipient_name`, `recipient_signature_ref`,
--     `recipient_signed_at`): the logistics proof carries a
--     signature storage reference but no signer name or
--     signed-at time. Signature bytes live in the storage
--     layer; only the reference is stored here.
--   - Unique indexes enforce the module invariants in the
--     database too: one DO number per tenant, and one item
--     row per (delivery note, product, description) is NOT
--     enforced - a DO may legitimately carry two lines for
--     the same product (different notes or units) - so the
--     item table carries no uniqueness beyond its id.
--   - This migration seeds no rows: a tenant with no settings
--     row uses the module defaults (prefix `SJ`, width 4,
--     yearly reset, proof required for delivery, auto-advance
--     on), and a tenant with no delivery notes has no numbers
--     issued. Only numeric / boolean knobs have defaults, in
--     `delivery_note_settings` below.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Delivery notes: one DO document. `number` is the human DO
-- number (for example `SJ-2026-0001`), unique per tenant.
-- `shipment_id` links the logistics shipment by id only;
-- `shipment_link_status` records whether logistics confirmed
-- the link (`linked`) or not (`unverified`, `none`).
-- `order_id` / `rental_order_id` link a store order / rental
-- order by id only. `company_id` links the issuing company
-- by id only. Sender and receiver are snapshots (name,
-- address, phone, email); driver, vehicle, and warehouse are
-- snapshots too (`driver_name`, `vehicle_plate`,
-- `vehicle_type`, `warehouse_name` plus their id references).
-- `tracking_number` is the shipment's tracking number as
-- snapshotted at creation time. `proof_of_delivery_id` links
-- the logistics proof of delivery by id only; the recipient
-- signature metadata lives on this row.
CREATE TABLE IF NOT EXISTS delivery_notes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    number TEXT NOT NULL CHECK (char_length(number) BETWEEN 1 AND 64),
    shipment_id UUID,
    shipment_link_status TEXT NOT NULL DEFAULT 'none' CHECK (shipment_link_status IN ('linked', 'unverified', 'none')),
    order_id TEXT CHECK (order_id IS NULL OR char_length(order_id) BETWEEN 1 AND 200),
    rental_order_id TEXT CHECK (rental_order_id IS NULL OR char_length(rental_order_id) BETWEEN 1 AND 200),
    company_id UUID,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'issued', 'in_transit', 'delivered', 'returned', 'cancelled')),
    sender_name TEXT NOT NULL CHECK (char_length(sender_name) BETWEEN 1 AND 200),
    sender_address TEXT NOT NULL CHECK (char_length(sender_address) BETWEEN 1 AND 500),
    sender_phone TEXT CHECK (sender_phone IS NULL OR char_length(sender_phone) BETWEEN 1 AND 200),
    sender_email TEXT CHECK (sender_email IS NULL OR char_length(sender_email) BETWEEN 1 AND 200),
    receiver_name TEXT NOT NULL CHECK (char_length(receiver_name) BETWEEN 1 AND 200),
    receiver_address TEXT NOT NULL CHECK (char_length(receiver_address) BETWEEN 1 AND 500),
    receiver_phone TEXT CHECK (receiver_phone IS NULL OR char_length(receiver_phone) BETWEEN 1 AND 200),
    receiver_email TEXT CHECK (receiver_email IS NULL OR char_length(receiver_email) BETWEEN 1 AND 200),
    warehouse_id UUID,
    warehouse_name TEXT CHECK (warehouse_name IS NULL OR char_length(warehouse_name) BETWEEN 1 AND 200),
    driver_id UUID,
    driver_name TEXT CHECK (driver_name IS NULL OR char_length(driver_name) BETWEEN 1 AND 200),
    vehicle_id UUID,
    vehicle_plate TEXT CHECK (vehicle_plate IS NULL OR char_length(vehicle_plate) BETWEEN 1 AND 20),
    vehicle_type TEXT CHECK (vehicle_type IS NULL OR char_length(vehicle_type) BETWEEN 1 AND 200),
    tracking_number TEXT CHECK (tracking_number IS NULL OR char_length(tracking_number) BETWEEN 1 AND 200),
    proof_of_delivery_id UUID,
    recipient_name TEXT CHECK (recipient_name IS NULL OR char_length(recipient_name) BETWEEN 1 AND 200),
    recipient_signature_ref TEXT CHECK (recipient_signature_ref IS NULL OR char_length(recipient_signature_ref) BETWEEN 1 AND 200),
    recipient_signed_at TIMESTAMPTZ,
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    return_reason TEXT CHECK (return_reason IS NULL OR char_length(return_reason) BETWEEN 1 AND 500),
    return_scope TEXT NOT NULL DEFAULT 'none' CHECK (return_scope IN ('full', 'partial', 'none')),
    returned_at TIMESTAMPTZ,
    issued_at TIMESTAMPTZ,
    delivered_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_notes_tenant_number ON delivery_notes (tenant_id, number);
CREATE INDEX IF NOT EXISTS idx_delivery_notes_tenant_status ON delivery_notes (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_delivery_notes_tenant_shipment ON delivery_notes (tenant_id, shipment_id, created_at DESC) WHERE shipment_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_delivery_notes_tenant_rental ON delivery_notes (tenant_id, rental_order_id, created_at DESC) WHERE rental_order_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_delivery_notes_tenant_receiver ON delivery_notes (tenant_id, receiver_name, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_delivery_notes_tenant_company ON delivery_notes (tenant_id, company_id, created_at DESC) WHERE company_id IS NOT NULL;

-- Delivery note items: one line of one DO. `product_id`
-- references a product by id only (no foreign key).
-- `quantity_ordered` and `quantity_shipped` are frozen at
-- creation; only `quantity_returned` grows, through the
-- return endpoint, and it never exceeds `quantity_shipped`.
CREATE TABLE IF NOT EXISTS delivery_note_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    delivery_note_id UUID NOT NULL,
    product_id TEXT CHECK (product_id IS NULL OR char_length(product_id) BETWEEN 1 AND 200),
    description TEXT NOT NULL CHECK (char_length(description) BETWEEN 1 AND 200),
    quantity_ordered BIGINT NOT NULL CHECK (quantity_ordered > 0),
    quantity_shipped BIGINT NOT NULL CHECK (quantity_shipped >= 0),
    quantity_returned BIGINT NOT NULL DEFAULT 0 CHECK (quantity_returned >= 0),
    unit TEXT NOT NULL DEFAULT 'pcs' CHECK (char_length(unit) BETWEEN 1 AND 32),
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (quantity_shipped <= quantity_ordered),
    CHECK (quantity_returned <= quantity_shipped)
);
CREATE INDEX IF NOT EXISTS idx_delivery_note_items_tenant_note ON delivery_note_items (tenant_id, delivery_note_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_delivery_note_items_tenant_product ON delivery_note_items (tenant_id, product_id, created_at DESC) WHERE product_id IS NOT NULL;

-- Per-tenant delivery notes settings. The DO number format is
-- data: `number_prefix` + sequence, zero-padded to
-- `number_width`, restarting per `number_reset_rule`
-- (`yearly`, `monthly`, or `never`).
-- `require_proof_for_delivery` gates the delivered transition
-- on a linked proof of delivery;
-- `auto_advance_from_shipment` lets logistics shipment events
-- move DOs forward. Changing the format only affects numbers
-- issued after the change - issued numbers are never
-- rewritten, and the unique index above keeps them unique.
CREATE TABLE IF NOT EXISTS delivery_note_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    number_prefix TEXT NOT NULL DEFAULT 'SJ' CHECK (char_length(number_prefix) BETWEEN 1 AND 16),
    number_width BIGINT NOT NULL DEFAULT 4 CHECK (number_width BETWEEN 1 AND 8),
    number_reset_rule TEXT NOT NULL DEFAULT 'yearly' CHECK (number_reset_rule IN ('yearly', 'monthly', 'never')),
    default_template_id TEXT CHECK (default_template_id IS NULL OR char_length(default_template_id) BETWEEN 1 AND 128),
    default_paper_size_code TEXT CHECK (default_paper_size_code IS NULL OR char_length(default_paper_size_code) BETWEEN 1 AND 32),
    default_output_kind TEXT CHECK (default_output_kind IS NULL OR default_output_kind IN ('paper', 'pdf', 'thermal', 'email')),
    require_proof_for_delivery BOOLEAN NOT NULL DEFAULT TRUE,
    auto_advance_from_shipment BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
