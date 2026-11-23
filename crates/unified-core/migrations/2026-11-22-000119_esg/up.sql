-- ESG module (`src/modules/esg/`):
-- Environmental, Social, and Governance sustainability
-- reporting: admin-defined metric definitions (category,
-- unit, data type, reporting frequency, framework tags) and
-- reporting-framework definitions as data, reporting periods
-- with open / closed status, metric values per period per
-- scope (company / site reference ids) with source notes,
-- document references, and full value history, per-metric
-- targets per period with direction and deadline, and an
-- append-only audit trail of entry / target / period changes.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000118_spreadsheets:
-- the in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz.
--     `metric_id` / `period_id` / `entry_id` / `target_id`
--     reference ESG rows by id with NO foreign key, so this
--     module must not assume any other module's tables or
--     migration order. `created_by` / `entered_by` / `actor`
--     are plain TEXT with NO foreign key on purpose: users
--     live in the users module and this module must not assume
--     its table or migration order. `scope_type` / `scope_id`
--     are plain TEXT reference ids (a company or a site from
--     the company module, or any other scope the tenant uses):
--     the company module emits no lifecycle events, so there
--     is no foreign key and no existence check here.
--     `document_ids` are plain TEXT references into the
--     documents module by id only, also with NO foreign key.
--   - `tenant_id` is plain TEXT on every row: ESG data is
--     tenant business data and every query filters by it.
--     A row from another tenant reads as 404, never 403.
--   - Nothing is hard-coded: frameworks, metrics (including
--     their category, unit, data type, reporting frequency,
--     and framework tags), periods, targets, and scopes are
--     all data rows. The only fixed vocabulary is the ESG
--     taxonomy itself (`environmental`, `social`,
--     `governance`), the three value data types (`number`,
--     `text`, `boolean`), and the two target directions
--     (`increase`, `decrease`) — those are the shape of the
--     module, not its content.
--   - There is no floating point money or measurement in this
--     module: numeric entry and target values are stored as
--     canonical decimal strings (`esg_entries.value_text`,
--     `esg_targets.target_value`, scale preserved in the
--     string itself, e.g. `12.50` is stored as `12.5`), and
--     progress math runs on an integer-mantissa decimal type
--     in basis points (10000 = 100%) — `f64` is never used
--     for a stored or returned number anywhere in this module.
--     Boolean values are stored as the strings `true` /
--     `false`; text values are stored as written.
--   - Value history is append-only: every entry creation and
--     every update appends one `esg_entry_history` row with
--     who entered the value and when, so the full history of
--     a value is preserved even though the entry row itself
--     only holds the latest value. Audit rows are likewise
--     never updated or deleted through the API.
--   - Deleting a framework, metric, or period never deletes
--     the rows that reference it: entries, history, targets,
--     and audit rows stay on record, and the period summary
--     skips metrics that no longer exist.
--   - Unique indexes enforce the module invariants in the
--     database too: one period code per tenant, one entry per
--     (metric, period, scope), and one target per (metric,
--     period, scope).
--   - This migration seeds no rows: a tenant with no
--     framework or metric rows simply has nothing to report
--     against yet.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Reporting frameworks: admin-entered definitions of a
-- standard a tenant reports against, with its own version.
-- Frameworks are data, never a hard-coded list.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS esg_frameworks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    version TEXT CHECK (version IS NULL OR char_length(version) BETWEEN 1 AND 64),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_esg_frameworks_tenant_name ON esg_frameworks (tenant_id, name, created_at DESC);

-- ---------------------------------------------------------------------------
-- Metric definitions: one admin-defined sustainability
-- metric. `category` is the ESG taxonomy (`environmental`,
-- `social`, `governance`); `data_type` is `number`, `text`,
-- or `boolean`; `unit` is free text (a unit name the tenant
-- uses); `frequency` is free text (a cadence name) so no
-- cadence list is hard-coded; `framework_ids` is a JSON array
-- of framework ids the metric reports under (plain TEXT
-- references, no foreign key — a metric may tag a framework
-- that is created later, and deleting a framework never
-- deletes the metrics that tagged it). `active` false hides
-- the metric from new entries without deleting its history.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS esg_metrics (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    category TEXT NOT NULL CHECK (category IN ('environmental', 'social', 'governance')),
    unit TEXT NOT NULL CHECK (char_length(unit) BETWEEN 1 AND 64),
    data_type TEXT NOT NULL CHECK (data_type IN ('number', 'text', 'boolean')),
    frequency TEXT NOT NULL CHECK (char_length(frequency) BETWEEN 1 AND 64),
    framework_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_esg_metrics_tenant_category ON esg_metrics (tenant_id, category, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_esg_metrics_tenant_active ON esg_metrics (tenant_id, active, created_at DESC);

-- ---------------------------------------------------------------------------
-- Reporting periods: one period definition with a code (a
-- quarter or fiscal-year label), a date range, and an open /
-- closed status. Entries and targets may only be written
-- while a period is open; closing is explicit and reversible
-- through reopen. The code is unique per tenant.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS esg_periods (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 64),
    name TEXT CHECK (name IS NULL OR char_length(name) BETWEEN 1 AND 200),
    starts_on DATE NOT NULL,
    ends_on DATE NOT NULL CHECK (ends_on >= starts_on),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed')),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_esg_periods_tenant_code ON esg_periods (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_esg_periods_tenant_status ON esg_periods (tenant_id, status, starts_on DESC);

-- ---------------------------------------------------------------------------
-- Entries: one recorded metric value for a metric in a period
-- at a scope. `value_text` is the canonical decimal string
-- for numeric metrics, `true` / `false` for boolean metrics,
-- or the text as written. `scope_type` / `scope_id` are plain
-- reference ids (a company or a site, or any other scope the
-- tenant uses). `source_note` is the evidence note;
-- `document_ids` is a JSON array of document ids from the
-- documents module (references by id only, no document data
-- is copied here). One entry per (metric, period, scope).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS esg_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    metric_id UUID NOT NULL,
    period_id UUID NOT NULL,
    scope_type TEXT NOT NULL CHECK (char_length(scope_type) BETWEEN 1 AND 64),
    scope_id TEXT NOT NULL CHECK (char_length(scope_id) BETWEEN 1 AND 200),
    value_text TEXT NOT NULL CHECK (char_length(value_text) BETWEEN 1 AND 5000),
    source_note TEXT CHECK (source_note IS NULL OR char_length(source_note) BETWEEN 1 AND 5000),
    document_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    entered_by TEXT NOT NULL CHECK (char_length(entered_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_esg_entries_metric_period_scope ON esg_entries (metric_id, period_id, scope_type, scope_id);
CREATE INDEX IF NOT EXISTS idx_esg_entries_tenant_period ON esg_entries (tenant_id, period_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_esg_entries_tenant_metric ON esg_entries (tenant_id, metric_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Entry history: one row per recorded value. Every entry
-- creation and every update appends a row with who entered
-- the value and when, so the full history of a value is
-- preserved even though the entry row only holds the latest
-- value. History rows are never updated or deleted.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS esg_entry_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    entry_id UUID NOT NULL,
    metric_id UUID NOT NULL,
    period_id UUID NOT NULL,
    value_text TEXT NOT NULL CHECK (char_length(value_text) BETWEEN 1 AND 5000),
    source_note TEXT CHECK (source_note IS NULL OR char_length(source_note) BETWEEN 1 AND 5000),
    entered_by TEXT NOT NULL CHECK (char_length(entered_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_esg_entry_history_entry ON esg_entry_history (entry_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_esg_entry_history_tenant_metric ON esg_entry_history (tenant_id, metric_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Targets: one per-metric, per-period, per-scope target. The
-- target value uses the same storage as entries (canonical
-- decimal string, `true` / `false`, or free text).
-- `direction` names which way counts as progress
-- (`increase` for higher-is-better, `decrease` for
-- lower-is-better, e.g. emissions). One target per (metric,
-- period, scope); setting a target again replaces it, and
-- the replacement is recorded in the audit trail.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS esg_targets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    metric_id UUID NOT NULL,
    period_id UUID NOT NULL,
    scope_type TEXT NOT NULL CHECK (char_length(scope_type) BETWEEN 1 AND 64),
    scope_id TEXT NOT NULL CHECK (char_length(scope_id) BETWEEN 1 AND 200),
    target_value TEXT NOT NULL CHECK (char_length(target_value) BETWEEN 1 AND 5000),
    direction TEXT NOT NULL CHECK (direction IN ('increase', 'decrease')),
    deadline DATE NOT NULL,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_esg_targets_metric_period_scope ON esg_targets (metric_id, period_id, scope_type, scope_id);
CREATE INDEX IF NOT EXISTS idx_esg_targets_tenant_period ON esg_targets (tenant_id, period_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_esg_targets_tenant_metric ON esg_targets (tenant_id, metric_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Audit trail: one row per change to an entry, a target, or
-- a period (creates, updates, deletes, close / reopen).
-- `entity_type` names the row kind (`entry`, `target`,
-- `period`); `entity_id` is the row id as TEXT; `action`
-- names the change; `before` / `after` are the serialized
-- rows (or NULL for creates / deletes). Audit rows are
-- never updated or deleted through the API.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS esg_audit (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    entity_type TEXT NOT NULL CHECK (char_length(entity_type) BETWEEN 1 AND 64),
    entity_id TEXT NOT NULL CHECK (char_length(entity_id) BETWEEN 1 AND 200),
    action TEXT NOT NULL CHECK (char_length(action) BETWEEN 1 AND 64),
    actor TEXT NOT NULL CHECK (char_length(actor) BETWEEN 1 AND 200),
    before JSONB,
    after JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_esg_audit_tenant_created ON esg_audit (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_esg_audit_tenant_entity ON esg_audit (tenant_id, entity_type, entity_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_esg_audit_tenant_action ON esg_audit (tenant_id, action, created_at DESC);
