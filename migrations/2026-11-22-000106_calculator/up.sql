-- Calculator Enterprise tables for the Unified Pentagonal Project
-- (`src/modules/calculator.rs`): saved calculations per company,
-- shareable calculation reports, calculation history, and stored
-- currency rates. The module is a submodule of Company Management
-- (`requires = ["company"]`) and is not available as a product.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `calculator_saved_calculations.calculator_type` is `roi`,
--     `break_even`, `loan`, `depreciation`, `npv_irr`, `cash_flow`,
--     `pricing`, `salary`, `currency`, or `tax_estimate`
--     (`CALCULATOR_TYPES`). `inputs` and `results` are the exact
--     JSON objects the calculation ran with and produced.
--   - `calculator_reports.share_token` is a random 32-hex-character
--     token (never derived from the report id). `visibility` is
--     `private`, `shared`, or `public`; private reports may only be
--     read by their creator. Deleting a saved calculation deletes
--     its reports (`ON DELETE CASCADE`).
--   - `calculator_history` is the append-only audit trail of every
--     calculation run through the module, whether or not it was
--     saved. There is no delete path for history rows in the module.
--   - `calculator_currency_rates.rate` is DOUBLE PRECISION on
--     purpose and is NOT money: an exchange rate is a ratio between
--     two currencies, never a billed amount, and the module rounds
--     monetary results to four decimals (NUMERIC(19,4) rule) before
--     storing them in the JSONB `inputs`/`results` columns. No
--     other DOUBLE PRECISION column exists in this migration.
--     `max_age_seconds` is the configured staleness window for that
--     rate; a rate whose `fetched_at` is older than it is reported
--     with `stale: true` instead of being silently used as fresh.
--     One rate per (base_currency, target_currency, provider):
--     storing a new rate for the same triple replaces the old one.
--   - `company_id` is a plain UUID reference with NO foreign key on
--     purpose: the company tables are owned by the Company module
--     and deleting a company must not silently delete the audit
--     trail that answers what was calculated for it.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One calculation a company saved for later reuse.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS calculator_saved_calculations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL,
    calculator_type VARCHAR NOT NULL
        CHECK (calculator_type IN ('roi', 'break_even', 'loan', 'depreciation', 'npv_irr', 'cash_flow', 'pricing', 'salary', 'currency', 'tax_estimate')),
    title VARCHAR NOT NULL,
    inputs JSONB NOT NULL DEFAULT '{}'::jsonb,
    results JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_calculator_saved_calculations_company_id
    ON calculator_saved_calculations (company_id);
CREATE INDEX IF NOT EXISTS idx_calculator_saved_calculations_calculator_type
    ON calculator_saved_calculations (calculator_type);
CREATE INDEX IF NOT EXISTS idx_calculator_saved_calculations_created_by
    ON calculator_saved_calculations (created_by);
CREATE INDEX IF NOT EXISTS idx_calculator_saved_calculations_created_at
    ON calculator_saved_calculations (created_at);

-- ---------------------------------------------------------------------------
-- One shareable report built on a saved calculation.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS calculator_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    calculation_id UUID NOT NULL REFERENCES calculator_saved_calculations (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    company_id UUID NOT NULL,
    title VARCHAR NOT NULL,
    share_token VARCHAR NOT NULL,
    visibility VARCHAR NOT NULL DEFAULT 'private'
        CHECK (visibility IN ('private', 'shared', 'public')),
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_calculator_reports_share_token
    ON calculator_reports (share_token);
CREATE INDEX IF NOT EXISTS idx_calculator_reports_calculation_id
    ON calculator_reports (calculation_id);
CREATE INDEX IF NOT EXISTS idx_calculator_reports_company_id
    ON calculator_reports (company_id);
CREATE INDEX IF NOT EXISTS idx_calculator_reports_visibility
    ON calculator_reports (visibility);
CREATE INDEX IF NOT EXISTS idx_calculator_reports_created_by
    ON calculator_reports (created_by);

-- ---------------------------------------------------------------------------
-- One history entry per calculation run (append-only audit trail).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS calculator_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL,
    calculator_type VARCHAR NOT NULL
        CHECK (calculator_type IN ('roi', 'break_even', 'loan', 'depreciation', 'npv_irr', 'cash_flow', 'pricing', 'salary', 'currency', 'tax_estimate')),
    inputs JSONB NOT NULL DEFAULT '{}'::jsonb,
    results JSONB NOT NULL DEFAULT '{}'::jsonb,
    actor VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_calculator_history_company_id
    ON calculator_history (company_id);
CREATE INDEX IF NOT EXISTS idx_calculator_history_calculator_type
    ON calculator_history (calculator_type);
CREATE INDEX IF NOT EXISTS idx_calculator_history_actor
    ON calculator_history (actor);
CREATE INDEX IF NOT EXISTS idx_calculator_history_created_at
    ON calculator_history (created_at);

-- ---------------------------------------------------------------------------
-- One stored exchange rate (ratio, not money: DOUBLE PRECISION).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS calculator_currency_rates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    base_currency CHAR(3) NOT NULL
        CHECK (base_currency ~ '^[A-Z]{3}$'),
    target_currency CHAR(3) NOT NULL
        CHECK (target_currency ~ '^[A-Z]{3}$'),
    rate DOUBLE PRECISION NOT NULL CHECK (rate > 0),
    provider VARCHAR NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL,
    max_age_seconds INTEGER NOT NULL DEFAULT 3600 CHECK (max_age_seconds >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (base_currency <> target_currency)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_calculator_currency_rates_pair_provider
    ON calculator_currency_rates (base_currency, target_currency, provider);
CREATE INDEX IF NOT EXISTS idx_calculator_currency_rates_base_currency
    ON calculator_currency_rates (base_currency);
CREATE INDEX IF NOT EXISTS idx_calculator_currency_rates_target_currency
    ON calculator_currency_rates (target_currency);
CREATE INDEX IF NOT EXISTS idx_calculator_currency_rates_provider
    ON calculator_currency_rates (provider);
CREATE INDEX IF NOT EXISTS idx_calculator_currency_rates_fetched_at
    ON calculator_currency_rates (fetched_at);
