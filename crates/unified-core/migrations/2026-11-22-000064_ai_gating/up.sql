-- Layered AI gating for the Unified Pentagonal Project
-- (`src/modules/ai.rs`): the gate switches enforced by
-- `evaluate_ai_gate` on top of the batch-2 allowlists — a global
-- kill-switch plus global per-feature switches, and per-company /
-- per-user on-off switches plus per-feature off switches.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is new in this change set; run it in a
-- staging database first and confirm it applies cleanly before any
-- production run.
--
-- Notes on the mapping (the in-memory DashMap stores are still
-- authoritative; see the 000030_ai notes):
--   - `ai_company_policies.ai_enabled` is the company gate switch,
--     separate from the allowlist master switch `enabled`: the
--     effective company gate is `enabled AND ai_enabled`.
--     `disabled_features` is the JSON array of canonical feature
--     (use-case) names switched off for the company. NULL means no
--     feature is switched off here; an empty JSON array means the
--     same. Features follow the allowlist `allowed_features`
--     first, then this off-switch list.
--   - `ai_user_policies.ai_enabled` / `disabled_features` are the
--     same two switches for one user. A disabled user is denied
--     for every feature; a switched-off feature denies only that
--     feature.
--   - `ai_global_settings` holds one row per global setting, keyed
--     by setting name: `ai.global_enabled` (the kill-switch) and
--     `ai.features.<name>.enabled` (one row per globally switched
--     off feature; a row is created when an admin switches a
--     feature off and removed when it is switched back on, or its
--     `enabled` flag is flipped — either shape round-trips through
--     the in-memory `AiGlobalSettings`, which is authoritative).
--     The kill-switch row is seeded on so a fresh install behaves
--     exactly like before this migration.
--   - The integrator must add `ai_global_settings` to
--     `src/db/schema.rs` / `src/db/models.rs`; the two ALTERs only
--     add columns to tables those files already describe.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Company gate switches on the batch-2 allowlist table.
-- ---------------------------------------------------------------------------
ALTER TABLE ai_company_policies
    ADD COLUMN IF NOT EXISTS ai_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN IF NOT EXISTS disabled_features JSONB
        CHECK (disabled_features IS NULL OR jsonb_typeof(disabled_features) = 'array');

CREATE INDEX IF NOT EXISTS idx_ai_company_policies_ai_enabled
    ON ai_company_policies (ai_enabled);

-- ---------------------------------------------------------------------------
-- User gate switches on the batch-2 allowlist table.
-- ---------------------------------------------------------------------------
ALTER TABLE ai_user_policies
    ADD COLUMN IF NOT EXISTS ai_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN IF NOT EXISTS disabled_features JSONB
        CHECK (disabled_features IS NULL OR jsonb_typeof(disabled_features) = 'array');

CREATE INDEX IF NOT EXISTS idx_ai_user_policies_ai_enabled
    ON ai_user_policies (ai_enabled);

-- ---------------------------------------------------------------------------
-- Global AI settings: one row per setting, keyed by setting name.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_global_settings (
    key VARCHAR PRIMARY KEY,
    enabled BOOLEAN NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO ai_global_settings (key, enabled)
    VALUES ('ai.global_enabled', TRUE)
    ON CONFLICT (key) DO NOTHING;
