-- Spreadsheets module (`src/modules/spreadsheets/`):
-- spreadsheet workbooks with multiple sheets, each sheet a
-- grid of cells (A1 notation; value kinds text, number,
-- boolean, date, empty, formula, error), a small server-side
-- formula evaluator (arithmetic, cell references and ranges,
-- data-driven function registry), per-workbook sharing with
-- viewer / editor roles, token-gated public-link sharing that
-- is off by default, version history as full cell snapshots
-- with a change log, and CSV import / export plus JSON export.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000117_delivery_notes:
-- the in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz.
--     `workbook_id` / `sheet_id` reference spreadsheets rows
--     by id with NO foreign key, so this module must not
--     assume any other module's tables or migration order.
--     `created_by` / `owner_id` / `shared_with` are plain TEXT
--     with NO foreign key on purpose: users live in the users
--     module and this module must not assume its table or
--     migration order. `shared_with` names a user id; a share
--     may also carry `shared_with_email` for a person who has
--     no account yet.
--   - `tenant_id` is plain TEXT on every row: spreadsheets
--     are tenant business data and every query filters by it.
--     A row from another tenant reads as 404, never 403.
--   - Nothing is hard-coded: the formula function names live
--     in the evaluator's function registry (see
--     `src/modules/spreadsheets/formula.rs`, where the table
--     IS the data); number formats and share roles live in
--     `spreadsheet_settings`, one row per tenant; a workbook's
--     public link is a hashed token, never the raw token.
--   - There is no floating point money in this module: cell
--     numbers are stored as decimal strings
--     (`spreadsheet_cells.computed_value`, scale preserved in
--     the string itself, e.g. `12.50`), and the evaluator works
--     on an integer-mantissa decimal type — `f64` is never used
--     for a stored or returned number anywhere in this module.
--   - `spreadsheet_cells.kind` is `text`, `number`, `boolean`,
--     `date`, `empty`, `formula`, or `error`; a formula cell
--     keeps its formula text in `raw_value` (starting with
--     `=`) and its last computed result in `computed_value`
--     with `computed_kind` naming the result kind. An error
--     cell (or a formula whose evaluation failed) carries the
--     error token (`#VALUE!`, `#DIV/0!`, `#REF!`, `#NAME?`,
--     `#CIRC!`, `#NUM!`) in `computed_value`.
--   - Version history is a full snapshot per version, not a
--     diff: `spreadsheet_versions.cells_snapshot` is a JSON
--     array of `{address, kind, raw_value}` for every
--     non-empty cell of the sheet at that version, so a restore
--     reproduces the sheet exactly. `change_summary` is a
--     short human-readable log of the batch that created the
--     version (addresses changed and their before/after
--     values), kept for the audit view.
--   - Unique indexes enforce the module invariants in the
--     database too: one sheet title per workbook, one cell
--     per (sheet, address), one share per (workbook, user),
--     and one version number per (sheet).
--   - This migration seeds no rows: a tenant with no settings
--     row uses the module defaults (currency `USD`, number
--     formats `general`, `0.00`, `#,##0.00`, `percent`,
--     `currency`; share roles `viewer` and `editor`; public
--     links not allowed by default; at most 50 sheets per
--     workbook).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Spreadsheet workbooks: one document. `owner_id` is the user
-- id of the owner (plain TEXT, no foreign key). Public-link
-- sharing is off by default: `public_link_enabled` is false
-- and `public_token_hash` is empty until the owner enables it,
-- at which point the raw token is returned exactly once and
-- only its SHA-256 hash is stored.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS spreadsheets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    owner_id TEXT NOT NULL CHECK (char_length(owner_id) BETWEEN 1 AND 200),
    public_link_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    public_token_hash TEXT NOT NULL DEFAULT '' CHECK (char_length(public_token_hash) <= 128),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_spreadsheets_tenant_owner ON spreadsheets (tenant_id, owner_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_spreadsheets_tenant_updated ON spreadsheets (tenant_id, updated_at DESC);

-- ---------------------------------------------------------------------------
-- Sheets: one tab of one workbook. `position` orders the tabs;
-- `title` is unique per workbook.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS spreadsheet_sheets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    workbook_id UUID NOT NULL,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_spreadsheet_sheets_workbook_title ON spreadsheet_sheets (workbook_id, title);
CREATE INDEX IF NOT EXISTS idx_spreadsheet_sheets_tenant_workbook ON spreadsheet_sheets (tenant_id, workbook_id, position ASC);

-- ---------------------------------------------------------------------------
-- Cells: one cell of one sheet. `address` is A1 notation
-- (`B12`); `row_index` / `column_index` are zero-based so the
-- grid can be ranged without parsing addresses in SQL.
-- `raw_value` is what the user typed (a formula starts with
-- `=`); `computed_value` is the evaluated result as a decimal
-- string for numbers, `TRUE` / `FALSE` for booleans, an ISO
-- date for dates, or an error token. `number_format` names a
-- format from the tenant settings (data, not an enum).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS spreadsheet_cells (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    workbook_id UUID NOT NULL,
    sheet_id UUID NOT NULL,
    address TEXT NOT NULL CHECK (char_length(address) BETWEEN 2 AND 16),
    row_index BIGINT NOT NULL CHECK (row_index >= 0),
    column_index BIGINT NOT NULL CHECK (column_index >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('text', 'number', 'boolean', 'date', 'empty', 'formula', 'error')),
    raw_value TEXT NOT NULL DEFAULT '' CHECK (char_length(raw_value) <= 100000),
    computed_value TEXT NOT NULL DEFAULT '' CHECK (char_length(computed_value) <= 100000),
    computed_kind TEXT NOT NULL DEFAULT 'empty' CHECK (computed_kind IN ('text', 'number', 'boolean', 'date', 'empty', 'error')),
    number_format TEXT CHECK (number_format IS NULL OR char_length(number_format) BETWEEN 1 AND 64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_spreadsheet_cells_sheet_address ON spreadsheet_cells (sheet_id, address);
CREATE INDEX IF NOT EXISTS idx_spreadsheet_cells_tenant_sheet ON spreadsheet_cells (tenant_id, sheet_id, row_index ASC, column_index ASC);
CREATE INDEX IF NOT EXISTS idx_spreadsheet_cells_tenant_workbook ON spreadsheet_cells (tenant_id, workbook_id, updated_at DESC);

-- ---------------------------------------------------------------------------
-- Shares: one workbook shared with one user. `role` is data
-- (the tenant settings list the allowed roles; the module
-- defaults are `viewer` and `editor`). `shared_with` is a user
-- id; `shared_with_email` is kept for a person with no account
-- yet, and is anonymized when the user is deleted.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS spreadsheet_shares (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    workbook_id UUID NOT NULL,
    shared_with TEXT NOT NULL CHECK (char_length(shared_with) BETWEEN 1 AND 200),
    shared_with_email TEXT CHECK (shared_with_email IS NULL OR char_length(shared_with_email) BETWEEN 1 AND 254),
    role TEXT NOT NULL CHECK (char_length(role) BETWEEN 1 AND 64),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_spreadsheet_shares_workbook_user ON spreadsheet_shares (workbook_id, shared_with);
CREATE INDEX IF NOT EXISTS idx_spreadsheet_shares_tenant_workbook ON spreadsheet_shares (tenant_id, workbook_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_spreadsheet_shares_tenant_user ON spreadsheet_shares (tenant_id, shared_with, created_at DESC);

-- ---------------------------------------------------------------------------
-- Versions: one snapshot of one sheet. Every cell-write batch
-- creates one version; `version_number` counts up per sheet.
-- `cells_snapshot` is a JSON array of every non-empty cell
-- (`{address, kind, raw_value}`) so a restore reproduces the
-- sheet exactly. `change_summary` logs the batch that created
-- the version (addresses changed with before/after values).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS spreadsheet_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    workbook_id UUID NOT NULL,
    sheet_id UUID NOT NULL,
    version_number BIGINT NOT NULL CHECK (version_number >= 1),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    change_summary TEXT NOT NULL DEFAULT '' CHECK (char_length(change_summary) <= 100000),
    cells_snapshot JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_spreadsheet_versions_sheet_number ON spreadsheet_versions (sheet_id, version_number);
CREATE INDEX IF NOT EXISTS idx_spreadsheet_versions_tenant_sheet ON spreadsheet_versions (tenant_id, sheet_id, version_number DESC);

-- ---------------------------------------------------------------------------
-- Per-tenant spreadsheets settings. Number formats and share
-- roles are data: `number_formats` is a JSON array of
-- `{name, pattern}` (for example `{"name": "0.00", "pattern":
-- "0.00"}`), `share_roles` is a JSON array of role names, and
-- `default_currency` names the currency the `currency` format
-- renders. `public_links_allowed` is the tenant-wide switch
-- for public-link sharing (off by default); a workbook link
-- still needs to be enabled per workbook. Changing the
-- formats or roles only affects later writes — stored cells
-- keep the format name they were written with.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS spreadsheet_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    default_currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(default_currency) BETWEEN 1 AND 8),
    number_formats JSONB NOT NULL DEFAULT '[{"name": "general", "pattern": "general"}, {"name": "0.00", "pattern": "0.00"}, {"name": "#,##0.00", "pattern": "#,##0.00"}, {"name": "percent", "pattern": "percent"}, {"name": "currency", "pattern": "currency"}]'::jsonb,
    share_roles JSONB NOT NULL DEFAULT '["viewer", "editor"]'::jsonb,
    default_share_role TEXT NOT NULL DEFAULT 'viewer' CHECK (char_length(default_share_role) BETWEEN 1 AND 64),
    public_links_allowed BOOLEAN NOT NULL DEFAULT FALSE,
    max_sheets_per_workbook BIGINT NOT NULL DEFAULT 50 CHECK (max_sheets_per_workbook BETWEEN 1 AND 200),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
