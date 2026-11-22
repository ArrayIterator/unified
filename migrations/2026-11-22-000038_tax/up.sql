-- Tax Management module tables for the Unified Pentagonal Project
-- (`src/modules/tax.rs`): per-company tax rules, per-company tax
-- settings, stored tax computations, filing periods, provider
-- connectors (credentials write-only), and this module's own audit
-- trail. Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - Money is NUMERIC(19,4) everywhere: `rate_percent` is a
--     percentage (11.0000 means 11%), `threshold_amount`,
--     `base_amount`, `tax_total`, and `grand_total` are amounts.
--     The Rust side uses `f64` rounded to 4 decimal places, matching
--     the billing module's `f64` convention.
--   - `tax_rules.rate_percent` is a percentage, not a fraction: the
--     billing module stores per-line rates as fractions (0.10 means
--     10%), while tax rules store percentages (10.0000 means 10%).
--   - `tax_rules.threshold_amount` is NULL for "no threshold". A
--     rule applies only when the base amount is at least the
--     threshold and the computation date falls inside
--     `effective_from` .. `effective_to` (`effective_to` NULL means
--     open-ended).
--   - `tax_settings` has one row per company (`company_id`
--     UNIQUE). `filing_due_days_after_period` is the number of days
--     after a period ends before its filing is due.
--   - `tax_computations` has a UNIQUE (`source_type`, `source_id`)
--     pair: re-computing the same invoice or order replaces the
--     stored row instead of duplicating it, so a billing event that
--     arrives twice (real name and alias spelling) stores exactly
--     one computation. `lines` is the JSON array of `TaxLine`
--     records (rule id, name, type, rate, base, amount, inclusive,
--     compound_applied). The source document itself is never
--     mutated; this table is the tax module's own view.
--   - `tax_filings` has a UNIQUE (`company_id`, `country_code`,
--     `region_code`, `period_start`, `period_end`) combination.
--     `region_code` is NULL for country-wide filings; note that
--     PostgreSQL treats NULLs as distinct in a UNIQUE constraint,
--     so at most one country-wide filing per period is enforced by
--     the module, not by this constraint alone. `status` is
--     `upcoming`, `due`, `submitted`, or `overdue`.
--   - Credentials are write-only: `tax_connectors` stores only the
--     ciphertext from `encrypt_tax_credential` (a versioned `v1.`
--     HMAC keystream with an HMAC tag) and a short fingerprint
--     prefix, for the credential and for the HMAC signing secret
--     separately. `credential_encrypted` and
--     `credential_fingerprint` are both NULL when no credential is
--     set; the same holds for the signing-secret columns. No
--     response, log line, hook payload, or audit record ever
--     contains a credential or a fragment of one.
--   - `tax_connectors.kind` is `generic_http` or `example`
--     (`example` is a clearly-marked example provider mapping that
--     never performs a network call). `auth_scheme` is `none`,
--     `bearer`, `api_key`, or `hmac`. `allowed_hosts` is the JSON
--     array of hosts the endpoint URL may point at (SSRF
--     protection); `request_mapping` / `response_mapping` are JSON
--     objects mapping local field names to provider field names and
--     provider response paths to local field names.
--   - `tax_audit` is this module's own audit trail, recorded for
--     every tax record change whether or not the audit module lists
--     the `tax.*` events in its `AUDITED_EVENTS`. `details` never
--     carries a credential value, only fingerprints and flags.
--   - `tax_audit` carries `updated_at` although the
--     `TaxAuditEntry` struct has only `created_at`: the project
--     convention is created_at + updated_at on every table, and
--     updated_at normally equals created_at for an append-only
--     audit row.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One tax rule for one jurisdiction of one company.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS tax_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL,
    country_code VARCHAR(2) NOT NULL,
    region_code VARCHAR(16),
    name VARCHAR NOT NULL,
    tax_type VARCHAR NOT NULL
        CHECK (tax_type IN ('vat', 'gst', 'sales', 'use', 'withholding', 'excise', 'other')),
    rate_percent NUMERIC(19,4) NOT NULL
        CHECK (rate_percent >= 0 AND rate_percent <= 100),
    threshold_amount NUMERIC(19,4)
        CHECK (threshold_amount IS NULL OR threshold_amount >= 0),
    currency VARCHAR(3) NOT NULL,
    inclusive BOOLEAN NOT NULL DEFAULT FALSE,
    compound BOOLEAN NOT NULL DEFAULT FALSE,
    effective_from DATE NOT NULL,
    effective_to DATE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

CREATE INDEX IF NOT EXISTS idx_tax_rules_company_id
    ON tax_rules (company_id);
CREATE INDEX IF NOT EXISTS idx_tax_rules_country_code
    ON tax_rules (country_code);
CREATE INDEX IF NOT EXISTS idx_tax_rules_tax_type
    ON tax_rules (tax_type);
CREATE INDEX IF NOT EXISTS idx_tax_rules_enabled
    ON tax_rules (enabled);
CREATE INDEX IF NOT EXISTS idx_tax_rules_effective_from
    ON tax_rules (effective_from);

-- ---------------------------------------------------------------------------
-- One tax settings row per company.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS tax_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL UNIQUE,
    fiscal_year_start_month INTEGER NOT NULL DEFAULT 1
        CHECK (fiscal_year_start_month >= 1 AND fiscal_year_start_month <= 12),
    fiscal_year_start_day INTEGER NOT NULL DEFAULT 1
        CHECK (fiscal_year_start_day >= 1 AND fiscal_year_start_day <= 31),
    filing_frequency VARCHAR NOT NULL DEFAULT 'quarterly'
        CHECK (filing_frequency IN ('monthly', 'quarterly', 'yearly')),
    default_currency VARCHAR(3) NOT NULL DEFAULT 'USD',
    default_country_code VARCHAR(2),
    default_region_code VARCHAR(16),
    reminder_days_before INTEGER NOT NULL DEFAULT 7
        CHECK (reminder_days_before >= 0 AND reminder_days_before <= 90),
    filing_due_days_after_period INTEGER NOT NULL DEFAULT 20
        CHECK (filing_due_days_after_period >= 0 AND filing_due_days_after_period <= 365),
    notify_user_id VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_tax_settings_company_id
    ON tax_settings (company_id);

-- ---------------------------------------------------------------------------
-- One stored tax computation for an invoice, an order, or a manual
-- compute call. The source document is never mutated.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS tax_computations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL,
    source_type VARCHAR NOT NULL
        CHECK (source_type IN ('invoice', 'order', 'manual')),
    source_id VARCHAR NOT NULL,
    country_code VARCHAR(2),
    region_code VARCHAR(16),
    currency VARCHAR(3) NOT NULL,
    base_amount NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (base_amount >= 0),
    tax_total NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (tax_total >= 0),
    grand_total NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (grand_total >= 0),
    lines JSONB NOT NULL DEFAULT '[]'::jsonb,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_tax_computations_source
    ON tax_computations (source_type, source_id);
CREATE INDEX IF NOT EXISTS idx_tax_computations_company_id
    ON tax_computations (company_id);
CREATE INDEX IF NOT EXISTS idx_tax_computations_country_code
    ON tax_computations (country_code);
CREATE INDEX IF NOT EXISTS idx_tax_computations_computed_at
    ON tax_computations (computed_at);

-- ---------------------------------------------------------------------------
-- One filing period for one jurisdiction of one company.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS tax_filings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL,
    country_code VARCHAR(2) NOT NULL,
    region_code VARCHAR(16),
    period_start DATE NOT NULL,
    period_end DATE NOT NULL,
    frequency VARCHAR NOT NULL
        CHECK (frequency IN ('monthly', 'quarterly', 'yearly')),
    due_date DATE NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'upcoming'
        CHECK (status IN ('upcoming', 'due', 'submitted', 'overdue')),
    tax_total NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (tax_total >= 0),
    submitted_at TIMESTAMPTZ,
    submitted_via VARCHAR,
    reference VARCHAR,
    last_reminder_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (period_end >= period_start)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_tax_filings_period
    ON tax_filings (company_id, country_code, region_code, period_start, period_end);
CREATE INDEX IF NOT EXISTS idx_tax_filings_company_id
    ON tax_filings (company_id);
CREATE INDEX IF NOT EXISTS idx_tax_filings_status
    ON tax_filings (status);
CREATE INDEX IF NOT EXISTS idx_tax_filings_due_date
    ON tax_filings (due_date);

-- ---------------------------------------------------------------------------
-- One tax reporting / filing provider connection. Credentials are
-- write-only (ciphertext + fingerprint only).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS tax_connectors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL,
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('generic_http', 'example')),
    endpoint_url VARCHAR NOT NULL,
    allowed_hosts JSONB NOT NULL DEFAULT '[]'::jsonb,
    auth_scheme VARCHAR NOT NULL DEFAULT 'none'
        CHECK (auth_scheme IN ('none', 'bearer', 'api_key', 'hmac')),
    credential_encrypted TEXT,
    credential_fingerprint VARCHAR,
    signing_secret_encrypted TEXT,
    signing_secret_fingerprint VARCHAR,
    timeout_ms INTEGER NOT NULL DEFAULT 10000
        CHECK (timeout_ms >= 100 AND timeout_ms <= 120000),
    max_retries INTEGER NOT NULL DEFAULT 2
        CHECK (max_retries >= 0 AND max_retries <= 5),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    request_mapping JSONB NOT NULL DEFAULT '{}'::jsonb,
    response_mapping JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_tax_connectors_company_name
    ON tax_connectors (company_id, name);
CREATE INDEX IF NOT EXISTS idx_tax_connectors_company_id
    ON tax_connectors (company_id);
CREATE INDEX IF NOT EXISTS idx_tax_connectors_kind
    ON tax_connectors (kind);
CREATE INDEX IF NOT EXISTS idx_tax_connectors_enabled
    ON tax_connectors (enabled);

-- ---------------------------------------------------------------------------
-- This module's own audit trail: every tax record change, recorded
-- whether or not the audit module lists the `tax.*` events.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS tax_audit (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL,
    actor VARCHAR NOT NULL,
    action VARCHAR NOT NULL,
    resource_type VARCHAR NOT NULL,
    resource_id VARCHAR NOT NULL,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_tax_audit_company_id
    ON tax_audit (company_id);
CREATE INDEX IF NOT EXISTS idx_tax_audit_action
    ON tax_audit (action);
CREATE INDEX IF NOT EXISTS idx_tax_audit_resource_id
    ON tax_audit (resource_id);
CREATE INDEX IF NOT EXISTS idx_tax_audit_created_at
    ON tax_audit (created_at);
