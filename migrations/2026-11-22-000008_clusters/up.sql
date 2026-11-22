-- Multi-cluster sync tables for the Unified Pentagonal Project
-- (clusters.rs): clusters, their nodes, sync plans, the versioned
-- sync items, sync logs, and node health history.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `cluster_nodes.secret_hash` stores only the SHA-256 hash of
--     the node secret, never the secret itself; the masked
--     `secret_fingerprint` is the only secret-derived value ever
--     returned by the API. The same applies to
--     `cluster_sync_plans.peer_secret_hash`.
--   - `cluster_sync_items` is keyed by (domain, key): one versioned
--     item of local domain state per (domain, key), exactly like
--     the in-memory map. The payload is the item JSON.
--   - `cluster_sync_logs.conflicts` and `.errors` are JSON arrays
--     of the `SyncConflict` / error-string shapes from the code.
--   - Nonces seen within the replay window (`used_nonces` in the
--     code) are intentionally NOT persisted: they are short-lived
--     replay-protection state, not durable data.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- A cluster: another Unified instance group (`unified`) or a
-- provider pool (`provider_pool`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS clusters (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    kind VARCHAR NOT NULL CHECK (kind IN ('unified', 'provider_pool')),
    status VARCHAR NOT NULL CHECK (status IN ('active', 'paused', 'archived')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_clusters_status
    ON clusters (status);

-- ---------------------------------------------------------------------------
-- One node of a cluster. `role` is `primary`, `member`, or `edge`;
-- `status` is `active` or `revoked`; `health` is `healthy`,
-- `degraded`, `unhealthy`, or `unknown`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cluster_nodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cluster_id UUID NOT NULL REFERENCES clusters (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    display_name VARCHAR NOT NULL,
    base_url VARCHAR NOT NULL,
    role VARCHAR NOT NULL CHECK (role IN ('primary', 'member', 'edge')),
    status VARCHAR NOT NULL CHECK (status IN ('active', 'revoked')),
    secret_hash VARCHAR NOT NULL,
    secret_fingerprint VARCHAR NOT NULL,
    health VARCHAR NOT NULL DEFAULT 'unknown' CHECK (health IN ('healthy', 'degraded', 'unhealthy', 'unknown')),
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    last_seen_at TIMESTAMPTZ,
    last_latency_ms BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cluster_nodes_cluster_id
    ON cluster_nodes (cluster_id);
CREATE INDEX IF NOT EXISTS idx_cluster_nodes_health
    ON cluster_nodes (health);

-- ---------------------------------------------------------------------------
-- A sync plan: the peer node this plan syncs with, the direction,
-- the synced domains, and the schedule.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cluster_sync_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cluster_id UUID NOT NULL REFERENCES clusters (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    target_node_id UUID NOT NULL REFERENCES cluster_nodes (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    local_node_id VARCHAR NOT NULL,
    direction VARCHAR NOT NULL CHECK (direction IN ('push', 'pull', 'bidirectional')),
    domains JSONB NOT NULL DEFAULT '[]'::jsonb,
    schedule VARCHAR NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    peer_secret_hash VARCHAR NOT NULL,
    peer_secret_fingerprint VARCHAR,
    last_run_at TIMESTAMPTZ,
    last_success_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cluster_sync_plans_cluster_id
    ON cluster_sync_plans (cluster_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_plans_target_node_id
    ON cluster_sync_plans (target_node_id);

-- ---------------------------------------------------------------------------
-- One versioned item of local domain state, the unit sync moves.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cluster_sync_items (
    domain VARCHAR NOT NULL,
    key VARCHAR NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (domain, key)
);

CREATE INDEX IF NOT EXISTS idx_cluster_sync_items_domain
    ON cluster_sync_items (domain);

-- ---------------------------------------------------------------------------
-- One sync run log entry, with its conflicts and errors.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cluster_sync_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID REFERENCES cluster_sync_plans (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    cluster_id UUID NOT NULL REFERENCES clusters (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    node_id UUID REFERENCES cluster_nodes (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    direction VARCHAR NOT NULL CHECK (direction IN ('push', 'pull', 'bidirectional')),
    domain VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('completed', 'failed', 'dry_run')),
    items_sent BIGINT NOT NULL DEFAULT 0 CHECK (items_sent >= 0),
    items_received BIGINT NOT NULL DEFAULT 0 CHECK (items_received >= 0),
    items_applied BIGINT NOT NULL DEFAULT 0 CHECK (items_applied >= 0),
    items_skipped BIGINT NOT NULL DEFAULT 0 CHECK (items_skipped >= 0),
    conflicts JSONB NOT NULL DEFAULT '[]'::jsonb,
    errors JSONB NOT NULL DEFAULT '[]'::jsonb,
    dry_run BOOLEAN NOT NULL DEFAULT FALSE,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cluster_sync_logs_plan_id
    ON cluster_sync_logs (plan_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_logs_cluster_id
    ON cluster_sync_logs (cluster_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_logs_node_id
    ON cluster_sync_logs (node_id);
CREATE INDEX IF NOT EXISTS idx_cluster_sync_logs_status
    ON cluster_sync_logs (status);

-- ---------------------------------------------------------------------------
-- One node health-check observation.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cluster_health_checks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    node_id UUID NOT NULL REFERENCES cluster_nodes (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    cluster_id UUID NOT NULL REFERENCES clusters (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    status VARCHAR NOT NULL CHECK (status IN ('healthy', 'degraded', 'unhealthy', 'unknown')),
    latency_ms BIGINT,
    error TEXT,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cluster_health_checks_node_id
    ON cluster_health_checks (node_id);
CREATE INDEX IF NOT EXISTS idx_cluster_health_checks_cluster_id
    ON cluster_health_checks (cluster_id);
CREATE INDEX IF NOT EXISTS idx_cluster_health_checks_checked_at
    ON cluster_health_checks (checked_at);
