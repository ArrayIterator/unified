-- Cluster sync reliability tables for the Unified Pentagonal Project
-- (clusters.rs): first-class sync conflict records, the failed-sync
-- retry queue (including dead-letter entries), idempotency records for
-- admin writes, admin-editable sync settings, and the registered sync
-- domain registry.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `cluster_sync_conflicts` mirrors `SyncConflictRecord`: one row
--     per conflict, with both payloads so an admin resolution can
--     actually apply the chosen side. `status` is `open`, `resolved`
--     or `dismissed`; `resolution` is `keep_local`, `keep_remote` or
--     `dismiss`.
--   - `cluster_sync_retries` mirrors `SyncRetryEntry`: one row per
--     failed plan/domain. `status` is `pending`, `retrying`,
--     `completed`, `dead_letter` or `discarded`. A dead-letter entry
--     is never retried automatically.
--   - `cluster_sync_idempotency_keys` mirrors `IdempotencyRecord`:
--     one stored response per (scope, key), expiring at `expires_at`.
--   - `cluster_sync_settings` holds one row per setting key; the live
--     values are seeded from the configurable defaults in the code.
--   - `cluster_sync_domains` holds the registered sync domains,
--     seeded with config, modules, software, licenses and dns_ssl.
--   - The matching `schema.rs` / `models.rs` entries are integrator
--     work: those files are owned by the integration round.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One sync conflict an admin can see and resolve.
CREATE TABLE IF NOT EXISTS cluster_sync_conflicts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    log_id UUID REFERENCES cluster_sync_logs (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    plan_id UUID REFERENCES cluster_sync_plans (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    cluster_id UUID NOT NULL REFERENCES clusters (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    node_id UUID REFERENCES cluster_nodes (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    domain VARCHAR NOT NULL,
    key VARCHAR NOT NULL,
    local_updated_at TIMESTAMPTZ,
    remote_updated_at TIMESTAMPTZ,
    winner VARCHAR NOT NULL CHECK (winner IN ('local', 'remote', 'undecided')),
    reason TEXT NOT NULL DEFAULT '',
    local_payload JSONB,
    remote_payload JSONB,
    status VARCHAR NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved', 'dismissed')),
    resolution VARCHAR CHECK (resolution IS NULL OR resolution LIKE 'keep_local%' OR resolution LIKE 'keep_remote%' OR resolution LIKE 'dismiss%'),
    resolved_by VARCHAR,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cluster_sync_conflicts_cluster_id
    ON cluster_sync_conflicts (cluster_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_conflicts_plan_id
    ON cluster_sync_conflicts (plan_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_conflicts_status
    ON cluster_sync_conflicts (status);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_conflicts_domain
    ON cluster_sync_conflicts (domain);

-- One failed sync domain waiting for retry, or dead-lettered.
CREATE TABLE IF NOT EXISTS cluster_sync_retries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID NOT NULL REFERENCES cluster_sync_plans (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    cluster_id UUID NOT NULL REFERENCES clusters (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    node_id UUID REFERENCES cluster_nodes (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    domain VARCHAR NOT NULL,
    direction VARCHAR NOT NULL CHECK (direction IN ('push', 'pull', 'bidirectional')),
    attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt >= 1),
    max_attempts INTEGER NOT NULL DEFAULT 5 CHECK (max_attempts >= 1),
    next_retry_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error TEXT,
    status VARCHAR NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'retrying', 'completed', 'dead_letter', 'discarded')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cluster_sync_retries_plan_id
    ON cluster_sync_retries (plan_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_retries_cluster_id
    ON cluster_sync_retries (cluster_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_retries_status
    ON cluster_sync_retries (status);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_retries_next_retry_at
    ON cluster_sync_retries (next_retry_at);

-- One stored idempotent response, keyed by (scope, key).
CREATE TABLE IF NOT EXISTS cluster_sync_idempotency_keys (
    scope VARCHAR NOT NULL,
    key VARCHAR NOT NULL,
    status INTEGER NOT NULL CHECK (status >= 100 AND status < 600),
    body JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (scope, key)
);

CREATE INDEX IF NOT EXISTS idx_cluster_sync_idempotency_keys_expires_at
    ON cluster_sync_idempotency_keys (expires_at);

-- Admin-editable sync settings, one row per key. Values are JSON so
-- numbers, strings and booleans all fit; the code seeds these from its
-- configurable defaults.
CREATE TABLE IF NOT EXISTS cluster_sync_settings (
    key VARCHAR PRIMARY KEY,
    value JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO cluster_sync_settings (key, value) VALUES
    ('replay_window_secs', '300'::jsonb),
    ('health_timeout_secs', '5'::jsonb),
    ('sync_timeout_secs', '30'::jsonb),
    ('health_failure_threshold', '3'::jsonb),
    ('max_sync_logs', '1000'::jsonb),
    ('max_health_history', '5000'::jsonb),
    ('max_conflicts', '5000'::jsonb),
    ('max_retries', '5000'::jsonb),
    ('max_idempotency_records', '5000'::jsonb),
    ('retry_max_attempts', '5'::jsonb),
    ('retry_base_delay_secs', '60'::jsonb),
    ('retry_max_delay_secs', '3600'::jsonb),
    ('retry_backoff_multiplier', '2.0'::jsonb),
    ('idempotency_ttl_secs', '86400'::jsonb)
ON CONFLICT (key) DO NOTHING;

-- Registered sync domains. Seeded with the configurable defaults.
CREATE TABLE IF NOT EXISTS cluster_sync_domains (
    domain VARCHAR PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO cluster_sync_domains (domain) VALUES
    ('config'),
    ('modules'),
    ('software'),
    ('licenses'),
    ('dns_ssl')
ON CONFLICT (domain) DO NOTHING;
