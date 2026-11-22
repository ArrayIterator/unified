-- Card validation tables for the Unified project
-- (`src/modules/cards.rs`): administrator-defined custom brand
-- rules and daily validation counts. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Tenant scoping (standing rule): BOTH tables are TENANT business
-- data and carry `tenant_id` (uuidv7, as text) on every row. This
-- SQL is deliberately schema-agnostic (no schema qualification).
-- The migration runner sets `search_path` to the tenant schema
-- when it runs, so these tables are created inside the tenant
-- schema, never in `public`.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the payments / holidays precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `tenant_id` is a plain TEXT column with NO foreign key on
--     purpose: tenants live in another module and this module must
--     not assume their table or migration order. Referential rules
--     are enforced by the module code instead.
--   - NO PAN OR CVV IS STORED HERE, BY DESIGN. `card_custom_rules`
--     holds only IIN prefixes/ranges, allowed lengths, and CVV
--     length (rule data, never a card number).
--     `card_validation_daily_counts` holds only aggregate counts
--     keyed by (tenant, date, matched rule, result). There is no
--     column that could hold a card number, expiry, or CVV, and
--     none may ever be added: validation is stateless.
--   - `card_custom_rules.iin_prefixes` is a JSONB array of digit
--     strings (1-8 digits each); `iin_ranges` is a JSONB array of
--     `{"start", "end"}` digit strings of equal length (2-8);
--     `allowed_lengths` is a JSONB array of integers 12-19.
--     The module validates all three on write; the CHECKs below
--     only bound the CVV length and the override id length.
--   - `overrides_standard_brand` names a standard brand id
--     (for example `visa`); when set, the rule replaces that
--     standard brand's lengths / CVV length / Luhn requirement for
--     the tenant. It is nullable: standalone custom brands leave
--     it NULL.
--   - `card_validation_daily_counts` is keyed by
--     (tenant_id, date, matched_rule, result): one row per
--     (tenant, day, rule, outcome). `matched_rule` is
--     `standard:<brand id>`, `custom:<rule uuid>`, or `none`;
--     `result` is `valid` or `invalid`.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Custom brand rules (tenant business data).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS card_custom_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    brand_name TEXT NOT NULL CHECK (char_length(brand_name) BETWEEN 1 AND 100),
    iin_prefixes JSONB NOT NULL DEFAULT '[]'::jsonb,
    iin_ranges JSONB NOT NULL DEFAULT '[]'::jsonb,
    allowed_lengths JSONB NOT NULL DEFAULT '[]'::jsonb,
    cvv_length SMALLINT NOT NULL DEFAULT 3 CHECK (cvv_length IN (3, 4)),
    luhn_required BOOLEAN NOT NULL DEFAULT TRUE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    overrides_standard_brand TEXT CHECK (overrides_standard_brand IS NULL OR char_length(overrides_standard_brand) BETWEEN 1 AND 64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_card_custom_rules_tenant
    ON card_custom_rules (tenant_id);
CREATE INDEX IF NOT EXISTS idx_card_custom_rules_tenant_override
    ON card_custom_rules (tenant_id, overrides_standard_brand)
    WHERE overrides_standard_brand IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Daily validation counts (tenant business data, aggregates only).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS card_validation_daily_counts (
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    date DATE NOT NULL,
    matched_rule TEXT NOT NULL CHECK (char_length(matched_rule) BETWEEN 1 AND 128),
    result TEXT NOT NULL CHECK (result IN ('valid', 'invalid')),
    count BIGINT NOT NULL DEFAULT 0 CHECK (count >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, date, matched_rule, result)
);
CREATE INDEX IF NOT EXISTS idx_card_validation_daily_counts_tenant_date
    ON card_validation_daily_counts (tenant_id, date);
