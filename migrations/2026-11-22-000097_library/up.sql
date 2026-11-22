-- Library module tables for the Unified Pentagonal Project
-- (`src/modules/library.rs`): catalog items, copies, categories,
-- members, loans, holds, fines, loan policies, inventory audits,
-- discrepancies, and digital-access grants. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the e-learning precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` / `member_id` / `company_id` columns are plain
--     TEXT with NO foreign key on purpose: users and companies live
--     in other modules and this module must not assume their table
--     or migration order. Cross-table references inside this
--     module (`library_copies.item_id`, `library_loans.member_id`,
--     and so on) are also plain TEXT/UUID without foreign keys, so
--     the tables can be created and dropped in any order and a
--     missing parent row never blocks a write; referential rules
--     are enforced by the module code instead.
--   - Money is NUMERIC(19,4) (`library_fines.amount_minor` is
--     stored in minor units as NUMERIC so a fractional minor unit
--     can never be silently truncated; the module treats the value
--     as an integer number of minor units).
--   - `library_items.isbn_normalized` is the ISBN with hyphens
--     and spaces removed; uniqueness per edition is enforced by
--     the module (partial unique index below covers active items
--     only, matching the code rule that withdrawn items free the
--     ISBN for a new edition).
--   - `library_items.digital_ref`, `document_ref`, and
--     `storage_ref` are opaque references handed to the documents
--     / storage modules; no bytes are stored here.
--   - `library_digital_access` records issued grants; the token
--     itself is a stateless HMAC token and is never stored.
--   - No secret is stored in any table here.
--   - All ids are UUID, all timestamps are Timestamptz.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Catalog items.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 300),
    subtitle TEXT CHECK (subtitle IS NULL OR char_length(subtitle) BETWEEN 1 AND 300),
    authors JSONB NOT NULL DEFAULT '[]'::jsonb,
    isbn TEXT CHECK (isbn IS NULL OR char_length(isbn) BETWEEN 1 AND 32),
    isbn_normalized TEXT CHECK (isbn_normalized IS NULL OR char_length(isbn_normalized) BETWEEN 10 AND 13),
    edition TEXT CHECK (edition IS NULL OR char_length(edition) BETWEEN 1 AND 64),
    series TEXT CHECK (series IS NULL OR char_length(series) BETWEEN 1 AND 300),
    series_position BIGINT CHECK (series_position IS NULL OR series_position >= 0),
    publisher TEXT CHECK (publisher IS NULL OR char_length(publisher) BETWEEN 1 AND 300),
    published_year BIGINT CHECK (published_year IS NULL OR published_year BETWEEN 0 AND 2100),
    language TEXT NOT NULL CHECK (char_length(language) BETWEEN 1 AND 32),
    category_id UUID,
    category_name TEXT CHECK (category_name IS NULL OR char_length(category_name) BETWEEN 1 AND 64),
    item_type TEXT NOT NULL CHECK (char_length(item_type) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('active', 'withdrawn')),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    cover_ref TEXT CHECK (cover_ref IS NULL OR char_length(cover_ref) BETWEEN 1 AND 500),
    digital_ref TEXT CHECK (digital_ref IS NULL OR char_length(digital_ref) BETWEEN 1 AND 500),
    document_ref TEXT CHECK (document_ref IS NULL OR char_length(document_ref) BETWEEN 1 AND 500),
    storage_ref TEXT CHECK (storage_ref IS NULL OR char_length(storage_ref) BETWEEN 1 AND 500),
    tags JSONB NOT NULL DEFAULT '[]'::jsonb,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_items_status ON library_items (status);
CREATE INDEX IF NOT EXISTS idx_library_items_item_type ON library_items (item_type);
CREATE INDEX IF NOT EXISTS idx_library_items_language ON library_items (language);
CREATE INDEX IF NOT EXISTS idx_library_items_category ON library_items (category_id);
CREATE INDEX IF NOT EXISTS idx_library_items_isbn ON library_items (isbn_normalized);
CREATE UNIQUE INDEX IF NOT EXISTS idx_library_items_isbn_edition_active
    ON library_items (isbn_normalized, COALESCE(edition, '')) WHERE status = 'active' AND isbn_normalized IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Categories (admin-editable taxonomy).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_categories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 300),
    slug TEXT NOT NULL UNIQUE CHECK (char_length(slug) BETWEEN 1 AND 64),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    parent_id UUID,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_categories_parent ON library_categories (parent_id);
CREATE INDEX IF NOT EXISTS idx_library_categories_slug ON library_categories (slug);

-- ---------------------------------------------------------------------------
-- Copies (one row per physical copy, tracked by barcode).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_copies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    item_id UUID NOT NULL,
    barcode TEXT NOT NULL UNIQUE CHECK (char_length(barcode) BETWEEN 1 AND 64),
    serial TEXT CHECK (serial IS NULL OR char_length(serial) BETWEEN 1 AND 64),
    condition TEXT NOT NULL CHECK (char_length(condition) BETWEEN 1 AND 64),
    location TEXT CHECK (location IS NULL OR char_length(location) BETWEEN 1 AND 300),
    shelf TEXT CHECK (shelf IS NULL OR char_length(shelf) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('available', 'on-loan', 'reserved', 'lost', 'damaged', 'withdrawn')),
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 2000),
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_copies_item ON library_copies (item_id);
CREATE INDEX IF NOT EXISTS idx_library_copies_status ON library_copies (status);
CREATE INDEX IF NOT EXISTS idx_library_copies_shelf ON library_copies (shelf);
CREATE INDEX IF NOT EXISTS idx_library_copies_barcode ON library_copies (barcode);

-- ---------------------------------------------------------------------------
-- Members (user link only; user records live in the users module).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_members (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 500),
    member_type TEXT NOT NULL CHECK (char_length(member_type) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('active', 'suspended', 'expired')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 2000),
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_members_user ON library_members (user_id);
CREATE INDEX IF NOT EXISTS idx_library_members_type ON library_members (member_type);
CREATE INDEX IF NOT EXISTS idx_library_members_status ON library_members (status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_library_members_user_active
    ON library_members (user_id) WHERE status <> 'expired';

-- ---------------------------------------------------------------------------
-- Loans.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_loans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    member_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 500),
    item_id UUID NOT NULL,
    copy_id UUID,
    status TEXT NOT NULL CHECK (status IN ('active', 'overdue', 'returned', 'lost', 'damaged')),
    checked_out_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    due_at TIMESTAMPTZ NOT NULL,
    returned_at TIMESTAMPTZ,
    renewal_count BIGINT NOT NULL DEFAULT 0 CHECK (renewal_count >= 0),
    last_reminded_at TIMESTAMPTZ,
    last_overdue_noticed_at TIMESTAMPTZ,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_loans_member ON library_loans (member_id);
CREATE INDEX IF NOT EXISTS idx_library_loans_user ON library_loans (user_id);
CREATE INDEX IF NOT EXISTS idx_library_loans_item ON library_loans (item_id);
CREATE INDEX IF NOT EXISTS idx_library_loans_copy ON library_loans (copy_id);
CREATE INDEX IF NOT EXISTS idx_library_loans_status ON library_loans (status);
CREATE INDEX IF NOT EXISTS idx_library_loans_due ON library_loans (due_at);

-- ---------------------------------------------------------------------------
-- Holds (FIFO queue per item).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_holds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    member_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 500),
    item_id UUID NOT NULL,
    copy_id UUID,
    status TEXT NOT NULL CHECK (status IN ('queued', 'ready', 'fulfilled', 'cancelled', 'expired')),
    position BIGINT NOT NULL CHECK (position >= 1),
    ready_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_holds_member ON library_holds (member_id);
CREATE INDEX IF NOT EXISTS idx_library_holds_item ON library_holds (item_id);
CREATE INDEX IF NOT EXISTS idx_library_holds_status ON library_holds (status);
CREATE INDEX IF NOT EXISTS idx_library_holds_queue ON library_holds (item_id, position);

-- ---------------------------------------------------------------------------
-- Fines (billing owns the charge; this table records the request).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_fines (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    loan_id UUID NOT NULL,
    member_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 500),
    amount_minor NUMERIC(19,4) NOT NULL CHECK (amount_minor >= 0),
    currency TEXT NOT NULL CHECK (char_length(currency) = 3),
    reason TEXT NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('pending', 'paid', 'waived')),
    waived_by TEXT,
    paid_reference TEXT,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_fines_loan ON library_fines (loan_id);
CREATE INDEX IF NOT EXISTS idx_library_fines_member ON library_fines (member_id);
CREATE INDEX IF NOT EXISTS idx_library_fines_status ON library_fines (status);
CREATE INDEX IF NOT EXISTS idx_library_fines_reason ON library_fines (reason);

-- ---------------------------------------------------------------------------
-- Loan policies (all loan numbers are data).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_loan_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 300),
    member_type TEXT NOT NULL CHECK (char_length(member_type) BETWEEN 1 AND 64),
    item_type TEXT NOT NULL CHECK (char_length(item_type) BETWEEN 1 AND 64),
    loan_days BIGINT NOT NULL CHECK (loan_days >= 1),
    max_concurrent BIGINT NOT NULL CHECK (max_concurrent >= 1),
    max_renewals BIGINT NOT NULL CHECK (max_renewals >= 0),
    fine_per_day_minor NUMERIC(19,4) NOT NULL CHECK (fine_per_day_minor >= 0),
    currency TEXT NOT NULL CHECK (char_length(currency) = 3),
    grace_days BIGINT NOT NULL CHECK (grace_days >= 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_policies_member_type ON library_loan_policies (member_type);
CREATE INDEX IF NOT EXISTS idx_library_policies_item_type ON library_loan_policies (item_type);
CREATE UNIQUE INDEX IF NOT EXISTS idx_library_policies_pair_active
    ON library_loan_policies (member_type, item_type) WHERE active = TRUE;

-- ---------------------------------------------------------------------------
-- Inventory audits.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_inventory_audits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 300),
    scope_location TEXT CHECK (scope_location IS NULL OR char_length(scope_location) BETWEEN 1 AND 300),
    scope_shelf TEXT CHECK (scope_shelf IS NULL OR char_length(scope_shelf) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('open', 'completed')),
    started_by TEXT NOT NULL CHECK (char_length(started_by) BETWEEN 1 AND 500),
    completed_at TIMESTAMPTZ,
    items_checked BIGINT NOT NULL DEFAULT 0 CHECK (items_checked >= 0),
    discrepancies_found BIGINT NOT NULL DEFAULT 0 CHECK (discrepancies_found >= 0),
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_audits_status ON library_inventory_audits (status);
CREATE INDEX IF NOT EXISTS idx_library_audits_started_by ON library_inventory_audits (started_by);

-- ---------------------------------------------------------------------------
-- Inventory discrepancies.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_inventory_discrepancies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    audit_id UUID NOT NULL,
    copy_id UUID NOT NULL,
    item_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('missing', 'damaged', 'found')),
    note TEXT CHECK (note IS NULL OR char_length(note) BETWEEN 1 AND 2000),
    status TEXT NOT NULL CHECK (status IN ('pending', 'resolved', 'written_off')),
    resolved_by TEXT,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_discrepancies_audit ON library_inventory_discrepancies (audit_id);
CREATE INDEX IF NOT EXISTS idx_library_discrepancies_copy ON library_inventory_discrepancies (copy_id);
CREATE INDEX IF NOT EXISTS idx_library_discrepancies_item ON library_inventory_discrepancies (item_id);
CREATE INDEX IF NOT EXISTS idx_library_discrepancies_status ON library_inventory_discrepancies (status);

-- ---------------------------------------------------------------------------
-- Digital-access grants (token itself is never stored).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS library_digital_access (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    loan_id UUID NOT NULL,
    item_id UUID NOT NULL,
    member_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 500),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    redeemed_at TIMESTAMPTZ,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_library_digital_loan ON library_digital_access (loan_id);
CREATE INDEX IF NOT EXISTS idx_library_digital_item ON library_digital_access (item_id);
CREATE INDEX IF NOT EXISTS idx_library_digital_member ON library_digital_access (member_id);
CREATE INDEX IF NOT EXISTS idx_library_digital_expires ON library_digital_access (expires_at);
