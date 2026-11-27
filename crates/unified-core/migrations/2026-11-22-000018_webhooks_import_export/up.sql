-- Webhook, import, and export tables for the Unified Pentagonal
-- Project (webhooks.rs, import_export.rs): subscribed webhook
-- endpoints with their delivery attempts, and import / export job
-- records.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `webhook_endpoints.secret` is the HMAC signing secret. It
--     is never returned by any API response; the stored value must
--     be encrypted before insert, the same reservation as the
--     `*_encrypted` columns of `vcs_provider_settings` (no
--     encryption helper exists yet).
--   - `webhook_deliveries.payload` is kept so a failed delivery
--     can be redelivered manually with the exact same body;
--     `payload_hash` is the SHA-256 hex digest of the serialized
--     payload body. `status` is `queued`, `delivering`,
--     `delivered`, `failed`, or `retrying`.
--   - `import_jobs.staged_rows` from the `ImportJob` struct is NOT
--     stored: the struct marks it never-serialized, and it holds
--     validated rows only until confirmation stages them. The
--     `row_errors` and `results` arrays are stored as JSON.
--     `status` is `validated`, `failed`, or `staged`.
--   - `export_jobs.content` from the `ExportJob` struct is NOT
--     stored: finished export bytes are kept in memory for
--     download only. `download_url` is stored instead. `status`
--     is `completed` or `failed`.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One subscribed webhook endpoint. The signing secret is never
-- serialized.
CREATE TABLE IF NOT EXISTS webhook_endpoints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    url VARCHAR NOT NULL,
    events JSONB NOT NULL DEFAULT '[]'::jsonb,
    secret VARCHAR NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    paused BOOLEAN NOT NULL DEFAULT FALSE,
    max_attempts INTEGER NOT NULL DEFAULT 3 CHECK (max_attempts >= 1),
    backoff_base_secs BIGINT NOT NULL DEFAULT 60 CHECK (backoff_base_secs >= 0),
    timeout_secs BIGINT NOT NULL DEFAULT 30 CHECK (timeout_secs > 0),
    delivery_count BIGINT NOT NULL DEFAULT 0 CHECK (delivery_count >= 0),
    last_delivery_at TIMESTAMPTZ,
    last_status VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_webhook_endpoints_active
    ON webhook_endpoints (active);

-- One webhook delivery attempt record.
CREATE TABLE IF NOT EXISTS webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    endpoint_id UUID NOT NULL REFERENCES webhook_endpoints (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    event VARCHAR NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    payload_hash VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('queued', 'delivering', 'delivered', 'failed', 'retrying')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 3 CHECK (max_attempts >= 1),
    next_retry_at TIMESTAMPTZ,
    status_code INTEGER CHECK (status_code >= 100 AND status_code < 600),
    duration_ms BIGINT CHECK (duration_ms >= 0),
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_endpoint_id
    ON webhook_deliveries (endpoint_id);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_status
    ON webhook_deliveries (status);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_event
    ON webhook_deliveries (event);

-- One import job: a validated (and optionally staged) file import
-- for one entity.
CREATE TABLE IF NOT EXISTS import_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity VARCHAR NOT NULL,
    format VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('validated', 'failed', 'staged')),
    total_rows BIGINT NOT NULL DEFAULT 0 CHECK (total_rows >= 0),
    valid_rows BIGINT NOT NULL DEFAULT 0 CHECK (valid_rows >= 0),
    invalid_rows BIGINT NOT NULL DEFAULT 0 CHECK (invalid_rows >= 0),
    row_errors JSONB NOT NULL DEFAULT '[]'::jsonb,
    results JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_import_jobs_entity
    ON import_jobs (entity);
CREATE INDEX IF NOT EXISTS idx_import_jobs_status
    ON import_jobs (status);

-- One export job: a generated export file for one entity.
CREATE TABLE IF NOT EXISTS export_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity VARCHAR NOT NULL,
    format VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('completed', 'failed')),
    fields JSONB NOT NULL DEFAULT '[]'::jsonb,
    row_count BIGINT NOT NULL DEFAULT 0 CHECK (row_count >= 0),
    data_provider VARCHAR NOT NULL,
    download_url VARCHAR NOT NULL,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_export_jobs_entity
    ON export_jobs (entity);
CREATE INDEX IF NOT EXISTS idx_export_jobs_status
    ON export_jobs (status);
