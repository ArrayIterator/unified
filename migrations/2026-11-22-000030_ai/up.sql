-- AI module tables for the Unified Pentagonal Project
-- (`src/modules/ai.rs`): provider connections (credentials
-- write-only), AI-generated drafts a human must approve, and the
-- usage audit trail for every provider call.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - Credentials are write-only: `ai_provider_connections`
--     stores only the ciphertext from `encrypt_api_key` (a
--     versioned `v1.` HMAC keystream with an HMAC tag) and a short
--     fingerprint prefix. No response, log line, hook payload, or
--     usage record ever contains a key or a fragment of one.
--     `api_key_encrypted` and `key_fingerprint` are both NULL when
--     no key is set (a local endpoint may run without one).
--   - `ai_provider_connections.kind` is `anthropic`, `openai`,
--     `openai_compatible`, or `local` (`AiProviderKind`).
--     `rate_limit_per_minute` defaults to 60 and is capped at
--     10000 in the module (`MAX_RATE_LIMIT_PER_MINUTE`); the module
--     enforces only the upper bound, and this migration additionally
--     rejects 0, which would mean "no calls allowed" and is never
--     a meaningful configured limit.
--   - `ai_drafts.kind` is `support_reply`, `chat_reply`, `content`,
--     `email`, `kb_answer`, or `generic` (`DraftKind`); `status` is
--     `draft`, `approved`, or `rejected` (`DraftStatus`). Approval
--     only flips the status and emits `ai.draft.approved`: there is
--     NO publish or send path in this module. `ai_generated` is
--     always true. `sources` is the JSON array of `GroundingSource`
--     records (article id, title, URL, score, excerpt).
--     `provider_kind` repeats the provider kind denormalized (the
--     draft keeps its kind even if the connection is deleted, which
--     is why `provider_id` is ON DELETE SET NULL). `usage_id`
--     references `ai_usage_records`, created above this table.
--   - `ai_usage_records` is the audit trail for every provider
--     call, recorded whether it succeeded or failed. `provider_id`
--     is a plain UUID reference with NO foreign key on purpose:
--     deleting a provider connection must not delete the audit
--     trail that answers cost and rate-limit questions.
--     `use_case` is `complete`, `support_reply`, `chat_reply`,
--     `content`, `email`, `kb_answer`, or `test`; `status` is
--     `success` or `error`.
--   - `ai_usage_records.estimated_cost_usd` is DOUBLE PRECISION on
--     purpose and is NOT money: it is a floating-point estimate
--     computed from the provider's reported token counts and the
--     module's list-price table (`estimate_cost_usd` in ai.rs).
--     The NUMERIC(19,4) rule applies to money; an estimate that is
--     never billed, never summed for accounting, and rounded to six
--     decimals by the module stays a DOUBLE, matching the `f64` in
--     the struct. No other DOUBLE PRECISION column exists in this
--     migration.
--   - `ai_usage_records` carries `updated_at` although the
--     `AiUsageRecord` struct has only `created_at`: the project
--     convention is created_at + updated_at on every table, and
--     updated_at normally equals created_at for an append-only
--     audit row.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One stored AI provider connection. Credentials are write-only.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_provider_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('anthropic', 'openai', 'openai_compatible', 'local')),
    base_url VARCHAR NOT NULL,
    model VARCHAR NOT NULL,
    api_key_encrypted TEXT,
    key_fingerprint VARCHAR,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 60
        CHECK (rate_limit_per_minute >= 1 AND rate_limit_per_minute <= 10000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_ai_provider_connections_name
    ON ai_provider_connections (name);
CREATE INDEX IF NOT EXISTS idx_ai_provider_connections_kind
    ON ai_provider_connections (kind);
CREATE INDEX IF NOT EXISTS idx_ai_provider_connections_enabled
    ON ai_provider_connections (enabled);

-- ---------------------------------------------------------------------------
-- One provider call, recorded whether it succeeded or failed, so
-- cost and rate-limit questions have an audit trail.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_usage_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_id UUID NOT NULL,
    provider_kind VARCHAR NOT NULL
        CHECK (provider_kind IN ('anthropic', 'openai', 'openai_compatible', 'local')),
    actor VARCHAR NOT NULL,
    use_case VARCHAR NOT NULL
        CHECK (use_case IN ('complete', 'support_reply', 'chat_reply', 'content', 'email', 'kb_answer', 'test')),
    model VARCHAR NOT NULL,
    prompt_tokens INTEGER CHECK (prompt_tokens IS NULL OR prompt_tokens >= 0),
    completion_tokens INTEGER CHECK (completion_tokens IS NULL OR completion_tokens >= 0),
    total_tokens INTEGER CHECK (total_tokens IS NULL OR total_tokens >= 0),
    estimated_cost_usd DOUBLE PRECISION,
    latency_ms BIGINT NOT NULL DEFAULT 0 CHECK (latency_ms >= 0),
    status VARCHAR NOT NULL CHECK (status IN ('success', 'error')),
    error_code INTEGER CHECK (error_code IS NULL OR (error_code >= 100 AND error_code < 600)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_usage_records_provider_id
    ON ai_usage_records (provider_id);
CREATE INDEX IF NOT EXISTS idx_ai_usage_records_use_case
    ON ai_usage_records (use_case);
CREATE INDEX IF NOT EXISTS idx_ai_usage_records_status
    ON ai_usage_records (status);
CREATE INDEX IF NOT EXISTS idx_ai_usage_records_created_at
    ON ai_usage_records (created_at);

-- ---------------------------------------------------------------------------
-- One AI-generated draft. A human must approve it before anything is
-- sent or published; approval only flips the status.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ai_drafts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind VARCHAR NOT NULL
        CHECK (kind IN ('support_reply', 'chat_reply', 'content', 'email', 'kb_answer', 'generic')),
    title VARCHAR,
    subject VARCHAR,
    body TEXT NOT NULL,
    prompt TEXT,
    provider_id UUID REFERENCES ai_provider_connections (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    provider_kind VARCHAR NOT NULL
        CHECK (provider_kind IN ('anthropic', 'openai', 'openai_compatible', 'local')),
    model VARCHAR NOT NULL,
    actor VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'approved', 'rejected')),
    ai_generated BOOLEAN NOT NULL DEFAULT TRUE,
    target_type VARCHAR,
    target_id VARCHAR,
    sources JSONB NOT NULL DEFAULT '[]'::jsonb,
    usage_id UUID REFERENCES ai_usage_records (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    reviewed_by VARCHAR,
    reviewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_drafts_provider_id
    ON ai_drafts (provider_id);
CREATE INDEX IF NOT EXISTS idx_ai_drafts_kind
    ON ai_drafts (kind);
CREATE INDEX IF NOT EXISTS idx_ai_drafts_status
    ON ai_drafts (status);
CREATE INDEX IF NOT EXISTS idx_ai_drafts_actor
    ON ai_drafts (actor);
CREATE INDEX IF NOT EXISTS idx_ai_drafts_usage_id
    ON ai_drafts (usage_id);

