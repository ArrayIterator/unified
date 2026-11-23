-- AI access allowlists for the Unified Pentagonal Project
-- (`src/modules/ai.rs`): company and user access policies, personal
-- API keys (bring-your-own-key), and personal MCP servers, plus the
-- identity columns on the usage audit trail.
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
--   - `ai_company_policies` / `ai_user_policies` hold the
--     allowlists `authorize_ai_call` enforces. A NULL
--     `allowed_models` / `allowed_features` means "unrestricted"
--     for that dimension; an empty JSON array means "allow
--     nothing". A NULL cap means "no cap"; a user cap of NULL or 0
--     means "inherit the company cap" (the module, not the
--     database, applies that rule).
--   - `monthly_token_cap` is BIGINT (counts, not money).
--     `monthly_cost_cap_usd` is NUMERIC(19,4): it is money, and the
--     project rule is NUMERIC(19,4) for money.
--   - `ai_personal_keys` stores only the ciphertext from
--     `encrypt_api_key` and a short fingerprint prefix. No
--     response, log line, hook payload, or usage record ever
--     contains a key or a fragment of one. One key per user
--     (`user_id` UNIQUE): the key is scoped to its owning user.
--     `base_url` is only used by the key test for
--     OpenAI-compatible / local endpoints; live calls use the
--     provider connection's base URL.
--   - `ai_user_mcp_servers` stores only the ciphertext of the
--     optional auth token, same write-only rule. The endpoint URL
--     was validated at write time (https only, public host only);
--     the database does not re-validate it.
--   - The ALTERs on `ai_usage_records` add the identity columns
--     the 000030 table lacked: `user_id` / `company_id` are
--     nullable because legacy rows predate allowlists and because
--     some actors have no UUID user id; `denial_reason` carries
--     the non-leaking reason code for gate-denied calls;
--     `used_personal_key` marks calls that ran on the caller's
--     personal key. The foreign keys are ON DELETE SET NULL on
--     purpose: deleting a user or a company must not delete the
--     audit trail.
--   - `ai_usage_records.provider_id` keeps its deliberate lack of
--     a foreign key (see 000030_ai): denied calls record the
--     requested provider id, which may be the zero UUID when no
--     connection was resolved.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One company's AI access policy (one row per company).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_company_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL UNIQUE REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    allowed_models JSONB CHECK (allowed_models IS NULL OR jsonb_typeof(allowed_models) = 'array'),
    allowed_features JSONB CHECK (allowed_features IS NULL OR jsonb_typeof(allowed_features) = 'array'),
    monthly_token_cap BIGINT CHECK (monthly_token_cap IS NULL OR monthly_token_cap >= 0),
    monthly_cost_cap_usd NUMERIC(19,4)
        CHECK (monthly_cost_cap_usd IS NULL OR monthly_cost_cap_usd >= 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_company_policies_enabled
    ON ai_company_policies (enabled);

-- ---------------------------------------------------------------------------
-- One user's AI access policy (one row per user), set by an admin of
-- the user's company or by a platform admin.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_user_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL UNIQUE REFERENCES users (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    company_id UUID NOT NULL REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    allowed_models JSONB CHECK (allowed_models IS NULL OR jsonb_typeof(allowed_models) = 'array'),
    allowed_features JSONB CHECK (allowed_features IS NULL OR jsonb_typeof(allowed_features) = 'array'),
    monthly_token_cap BIGINT CHECK (monthly_token_cap IS NULL OR monthly_token_cap >= 0),
    monthly_cost_cap_usd NUMERIC(19,4)
        CHECK (monthly_cost_cap_usd IS NULL OR monthly_cost_cap_usd >= 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_user_policies_company_id
    ON ai_user_policies (company_id);
CREATE INDEX IF NOT EXISTS idx_ai_user_policies_enabled
    ON ai_user_policies (enabled);

-- ---------------------------------------------------------------------------
-- One user's own API key. Credentials are write-only.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_personal_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL UNIQUE REFERENCES users (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    provider_kind VARCHAR NOT NULL
        CHECK (provider_kind IN ('anthropic', 'openai', 'openai_compatible', 'local')),
    base_url VARCHAR,
    model VARCHAR,
    api_key_encrypted TEXT NOT NULL,
    key_fingerprint VARCHAR NOT NULL,
    monthly_token_cap BIGINT CHECK (monthly_token_cap IS NULL OR monthly_token_cap >= 0),
    monthly_cost_cap_usd NUMERIC(19,4)
        CHECK (monthly_cost_cap_usd IS NULL OR monthly_cost_cap_usd >= 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_personal_keys_provider_kind
    ON ai_personal_keys (provider_kind);
CREATE INDEX IF NOT EXISTS idx_ai_personal_keys_enabled
    ON ai_personal_keys (enabled);

-- ---------------------------------------------------------------------------
-- One user's own MCP servers. Auth tokens are write-only.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_user_mcp_servers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR NOT NULL,
    endpoint_url VARCHAR NOT NULL,
    auth_token_encrypted TEXT,
    token_fingerprint VARCHAR,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_ai_user_mcp_servers_user_name UNIQUE (user_id, name)
);

CREATE INDEX IF NOT EXISTS idx_ai_user_mcp_servers_user_id
    ON ai_user_mcp_servers (user_id);
CREATE INDEX IF NOT EXISTS idx_ai_user_mcp_servers_enabled
    ON ai_user_mcp_servers (enabled);

-- ---------------------------------------------------------------------------
-- Identity columns on the usage audit trail (000030_ai created this
-- table without them).
-- ---------------------------------------------------------------------------
ALTER TABLE ai_usage_records
    ADD COLUMN IF NOT EXISTS user_id UUID REFERENCES users (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    ADD COLUMN IF NOT EXISTS company_id UUID REFERENCES companies (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    ADD COLUMN IF NOT EXISTS denial_reason VARCHAR,
    ADD COLUMN IF NOT EXISTS used_personal_key BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_ai_usage_records_user_id
    ON ai_usage_records (user_id);
CREATE INDEX IF NOT EXISTS idx_ai_usage_records_company_id
    ON ai_usage_records (company_id);
