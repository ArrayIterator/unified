-- Domain Verification module tables for the Unified Pentagonal Project
-- (`src/modules/domain_verification.rs`): admin-determined DNS
-- verification settings (versioned rows), per-domain verification
-- tokens (SHA-256 hash only - the raw token is never stored), and the
-- DNS check attempt history. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the network_tools precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `alias_id` and `tenant_id` are plain UUID columns with NO
--     foreign key on purpose: aliases live in the SaaS Domains
--     module (`saas_domains`, batch 8 B1) and tenants are provisioned
--     by the SaaS core, so this module must not assume their tables
--     or migration order. `company_id` DOES reference
--     `companies (id)` (created by migration 000001), ON DELETE
--     CASCADE: deleting a company removes its tokens and attempts.
--   - `domain_verification_tokens.token_hash` holds the SHA-256 hex
--     digest of the raw token. The raw token is returned exactly once
--     in the issuance response and is never stored, logged, or put
--     in a hook event.
--   - `domain_verification_tokens.host` / `expected_value` /
--     `record_type` / `settings_version` are snapshotted at issuance:
--     a later admin settings change applies only to tokens issued
--     after the change and never rewrites what an already-issued
--     token must match. Changing the settings does NOT invalidate
--     already-verified domains.
--   - `domain_verification_settings` is versioned, not single-row:
--     every admin save inserts a new row with a higher `version`;
--     the live settings are the highest-version row. `value_template`
--     must contain `{token}` and `record_type` is `txt` or `cname`
--     (enforced by the module and by the CHECK constraints below).
--   - All ids are Uuid, all timestamps are Timestamptz. JSON columns
--     are JSONB: `domain_verification_attempts.looked_up`.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Admin-determined verification settings, versioned. One row per save;
-- the highest `version` row is live.
CREATE TABLE IF NOT EXISTS domain_verification_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version BIGINT NOT NULL,
    record_type TEXT NOT NULL CHECK (record_type IN ('txt', 'cname')),
    host_template TEXT NOT NULL CHECK (char_length(host_template) BETWEEN 1 AND 253),
    value_template TEXT NOT NULL CHECK (char_length(value_template) BETWEEN 1 AND 1000),
    token_length INTEGER NOT NULL CHECK (token_length BETWEEN 16 AND 128),
    token_alphabet TEXT NOT NULL CHECK (char_length(token_alphabet) >= 8),
    verify_timeout_seconds INTEGER NOT NULL CHECK (verify_timeout_seconds BETWEEN 1 AND 120),
    max_attempts INTEGER NOT NULL CHECK (max_attempts BETWEEN 1 AND 10),
    retry_delay_seconds INTEGER NOT NULL CHECK (retry_delay_seconds BETWEEN 0 AND 300),
    token_ttl_seconds BIGINT NOT NULL CHECK (token_ttl_seconds BETWEEN 300 AND 2592000),
    rate_limit_window_seconds BIGINT NOT NULL CHECK (rate_limit_window_seconds BETWEEN 60 AND 86400),
    updated_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (version)
);

-- Seed row: the configurable defaults (TXT record, 32-char token).
-- These defaults seed the store only; an admin save inserts a new
-- versioned row and replaces them entirely.
INSERT INTO domain_verification_settings (
    version, record_type, host_template, value_template,
    token_length, token_alphabet, verify_timeout_seconds, max_attempts,
    retry_delay_seconds, token_ttl_seconds, rate_limit_window_seconds,
    updated_by
) VALUES (
    1, 'txt', '_unified-verify', 'unified-verify={token}',
    32, 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789',
    10, 5, 10, 604800, 3600, 'system'
) ON CONFLICT (version) DO NOTHING;

-- One issued verification token per domain alias. Raw tokens are never
-- stored: only the SHA-256 hex digest (`token_hash`).
CREATE TABLE IF NOT EXISTS domain_verification_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain TEXT NOT NULL,
    alias_id UUID,
    tenant_id UUID,
    company_id UUID REFERENCES companies (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    record_type TEXT NOT NULL CHECK (record_type IN ('txt', 'cname')),
    host TEXT NOT NULL,
    expected_value TEXT NOT NULL,
    settings_version BIGINT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'verified', 'failed', 'expired', 'superseded', 'revoked')),
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    verified_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS domain_verification_tokens_domain_idx
    ON domain_verification_tokens (domain, issued_at DESC);
CREATE INDEX IF NOT EXISTS domain_verification_tokens_alias_idx
    ON domain_verification_tokens (alias_id, issued_at DESC);
CREATE INDEX IF NOT EXISTS domain_verification_tokens_status_idx
    ON domain_verification_tokens (status, expires_at);

-- One row per DNS check attempt inside a verify call.
CREATE TABLE IF NOT EXISTS domain_verification_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    token_id UUID NOT NULL REFERENCES domain_verification_tokens (id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    alias_id UUID,
    attempt_number INTEGER NOT NULL CHECK (attempt_number >= 1),
    result TEXT NOT NULL CHECK (result IN ('verified', 'pending', 'failed')),
    record_found TEXT,
    looked_up JSONB NOT NULL DEFAULT '{}'::jsonb,
    error TEXT,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS domain_verification_attempts_domain_idx
    ON domain_verification_attempts (domain, checked_at DESC);
CREATE INDEX IF NOT EXISTS domain_verification_attempts_token_idx
    ON domain_verification_attempts (token_id, attempt_number);
