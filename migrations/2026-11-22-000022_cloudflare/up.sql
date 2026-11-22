-- Cloudflare connector tables for the Unified Pentagonal Project
-- (cloudflare.rs): the connector settings and the cache-purge
-- history.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `cloudflare_settings` is a singleton row keyed by `key =
--     'global'`. The API token is write-only: it is stored in
--     `api_token_encrypted` and must be encrypted before insert,
--     the same reservation as the `*_encrypted` columns of
--     `vcs_provider_settings` (no encryption helper exists yet).
--     `account_id` and `default_zone_id` are the other settings
--     the connector reads.
--   - `cloudflare_purge_history` records every cache purge:
--     `kind` is `urls`, `prefixes`, or `everything`. The purge
--     targets are the JSON array of URLs / prefixes.
--   - The live rate-limit state (`CfRateLimitState`: 429 backoff
--     window, remaining quota, reset time) is intentionally NOT
--     persisted: it is transient, per-process enforcement state
--     that a restart must reset.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Cloudflare connector settings (singleton row).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cloudflare_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    api_token_encrypted TEXT NOT NULL DEFAULT '',
    account_id VARCHAR,
    default_zone_id VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- One cache-purge record.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cloudflare_purge_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    zone_id VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('urls', 'prefixes', 'everything')),
    targets JSONB NOT NULL DEFAULT '[]'::jsonb,
    status VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cloudflare_purge_history_zone_id
    ON cloudflare_purge_history (zone_id);
CREATE INDEX IF NOT EXISTS idx_cloudflare_purge_history_kind
    ON cloudflare_purge_history (kind);
CREATE INDEX IF NOT EXISTS idx_cloudflare_purge_history_created_at
    ON cloudflare_purge_history (created_at);
