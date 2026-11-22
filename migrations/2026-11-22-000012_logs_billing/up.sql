-- Central log viewer and billing tables for the Unified
-- Pentagonal Project (logs.rs, billing.rs): durable application /
-- access / error log entries, invoices with items, payments,
-- credit notes, billing settings, and the invoice number
-- sequences.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `app_logs` is the durable form of the `logs` table named in
--     the logs module header (the bare name `logs` is avoided: it
--     collides with the module name in generated code and with
--     common tooling). `level` is `trace`, `debug`, `info`, `warn`,
--     or `error`; `kind` is `application`, `access`, or `error`.
--     `context` is stored redacted, exactly as the module keeps
--     it: context keys that look like credentials are replaced
--     with `[redacted]` and credential fragments are scrubbed out
--     of messages before an entry is kept. Secrets are never
--     stored here.
--   - `app_logs.seq` is a monotonic sequence number unique across
--     the table. `updated_at` is added per the project convention
--     that every table carries both timestamps; it normally equals
--     `created_at`.
--   - `logs_settings` is a singleton row keyed by `key = 'global'`.
--   - Money is NUMERIC(19,4) everywhere below (see migration
--     000003): `invoices` totals, `invoice_items` prices,
--     `invoice_payments.amount`, and `credit_notes.amount`.
--   - `invoices.status` stores the base status (`draft`,
--     `issued`, `paid`, `partially_paid`, `unpaid`, `void`,
--     `credited`); the serialized `overdue` status is derived
--     from the due date and the outstanding balance, never stored
--     as source of truth. `amount_paid`, `credit_total`, and
--     `balance` are denormalized running totals maintained by the
--     module; `overdue` and `reminders_sent` follow the struct.
--   - `invoices.user_id` is VARCHAR, not a foreign key: the
--     billing module uses string user ids, the same reservation
--     as the `store_orders.user_id` follow-up in docs/SCHEMA.md.
--     `order_id` is VARCHAR for the same reason (store order ids
--     are UUIDs in `store_orders`, but the billing module carries
--     them as strings).
--   - `invoice_sequences` holds one row per number sequence
--     (`invoice`, `credit_note`): the monotonic counters that are
--     never reused and never reset.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One log entry: application, access, or error log, with redacted
-- structured context.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS app_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    seq BIGINT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT now(),
    level VARCHAR NOT NULL CHECK (level IN ('trace', 'debug', 'info', 'warn', 'error')),
    kind VARCHAR NOT NULL CHECK (kind IN ('application', 'access', 'error')),
    source VARCHAR NOT NULL,
    message TEXT NOT NULL,
    context JSONB NOT NULL DEFAULT '{}'::jsonb,
    request_id VARCHAR,
    user_id VARCHAR,
    method VARCHAR,
    path VARCHAR,
    status INTEGER CHECK (status >= 100 AND status < 600),
    duration_ms BIGINT CHECK (duration_ms >= 0),
    ip VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_app_logs_seq
    ON app_logs (seq);
CREATE INDEX IF NOT EXISTS idx_app_logs_level
    ON app_logs (level);
CREATE INDEX IF NOT EXISTS idx_app_logs_kind
    ON app_logs (kind);
CREATE INDEX IF NOT EXISTS idx_app_logs_source
    ON app_logs (source);
CREATE INDEX IF NOT EXISTS idx_app_logs_timestamp
    ON app_logs (timestamp);
CREATE INDEX IF NOT EXISTS idx_app_logs_request_id
    ON app_logs (request_id);

-- ---------------------------------------------------------------------------
-- Log viewer settings (singleton row): retention in days and the
-- maximum number of entries kept.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS logs_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    retention_days INTEGER NOT NULL DEFAULT 30 CHECK (retention_days >= 0),
    max_entries BIGINT NOT NULL DEFAULT 10000 CHECK (max_entries > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- An invoice, built from an order or directly. The number is
-- unique and never reused; the sequence is monotonic.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invoices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    number VARCHAR NOT NULL,
    order_id VARCHAR,
    user_id VARCHAR NOT NULL,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    subtotal NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (subtotal >= 0),
    tax_total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (tax_total >= 0),
    total NUMERIC(19, 4) NOT NULL CHECK (total >= 0),
    currency VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('draft', 'issued', 'paid', 'partially_paid', 'unpaid', 'void', 'credited')),
    overdue BOOLEAN NOT NULL DEFAULT FALSE,
    tax_inclusive BOOLEAN NOT NULL DEFAULT FALSE,
    amount_paid NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (amount_paid >= 0),
    credit_total NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (credit_total >= 0),
    balance NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (balance >= 0),
    issue_date TIMESTAMPTZ,
    due_date TIMESTAMPTZ,
    payment_terms_days INTEGER CHECK (payment_terms_days >= 0),
    paid_at TIMESTAMPTZ,
    notes TEXT,
    reminders_sent JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_invoices_number
    ON invoices (number);
CREATE INDEX IF NOT EXISTS idx_invoices_order_id
    ON invoices (order_id);
CREATE INDEX IF NOT EXISTS idx_invoices_user_id
    ON invoices (user_id);
CREATE INDEX IF NOT EXISTS idx_invoices_company_id
    ON invoices (company_id);
CREATE INDEX IF NOT EXISTS idx_invoices_status
    ON invoices (status);
CREATE INDEX IF NOT EXISTS idx_invoices_due_date
    ON invoices (due_date);

-- ---------------------------------------------------------------------------
-- One line of an invoice. `tax_amount` is the tax for the line;
-- `line_total` is the net amount when tax is exclusive and the
-- gross amount when tax is inclusive.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invoice_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id UUID NOT NULL REFERENCES invoices (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity >= 0),
    unit_price NUMERIC(19, 4) NOT NULL CHECK (unit_price >= 0),
    line_total NUMERIC(19, 4) NOT NULL CHECK (line_total >= 0),
    tax_rate DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (tax_rate >= 0 AND tax_rate <= 1),
    tax_amount NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (tax_amount >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_invoice_items_invoice_id
    ON invoice_items (invoice_id);

-- ---------------------------------------------------------------------------
-- One payment recorded against an invoice. Partial payments are
-- normal: each one updates the invoice balance and status.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invoice_payments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id UUID NOT NULL REFERENCES invoices (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    amount NUMERIC(19, 4) NOT NULL CHECK (amount > 0),
    method VARCHAR NOT NULL,
    reference VARCHAR,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_invoice_payments_invoice_id
    ON invoice_payments (invoice_id);

-- ---------------------------------------------------------------------------
-- A credit note issued against an invoice. It reduces the invoice
-- balance; when the balance reaches zero the invoice reads as
-- `credited`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS credit_notes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id UUID NOT NULL REFERENCES invoices (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    number VARCHAR NOT NULL,
    amount NUMERIC(19, 4) NOT NULL CHECK (amount > 0),
    reason TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_credit_notes_number
    ON credit_notes (number);
CREATE INDEX IF NOT EXISTS idx_credit_notes_invoice_id
    ON credit_notes (invoice_id);

-- ---------------------------------------------------------------------------
-- Billing configuration (singleton row): invoice number prefix,
-- default tax rate, tax-inclusive handling, payment terms, and the
-- reminder schedules (days before / after the due date).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS billing_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    invoice_prefix VARCHAR NOT NULL DEFAULT 'INV',
    default_tax_rate DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (default_tax_rate >= 0 AND default_tax_rate <= 1),
    tax_inclusive BOOLEAN NOT NULL DEFAULT FALSE,
    payment_terms_days INTEGER NOT NULL DEFAULT 30 CHECK (payment_terms_days >= 0),
    reminder_before_days JSONB NOT NULL DEFAULT '[7, 3, 1]'::jsonb,
    reminder_after_days JSONB NOT NULL DEFAULT '[1, 7, 14]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Monotonic number sequences: one row per sequence name
-- (`invoice`, `credit_note`). `next_value` is the next number to
-- hand out; it is never reused and never reset.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invoice_sequences (
    name VARCHAR PRIMARY KEY CHECK (name IN ('invoice', 'credit_note')),
    next_value BIGINT NOT NULL DEFAULT 1 CHECK (next_value >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
