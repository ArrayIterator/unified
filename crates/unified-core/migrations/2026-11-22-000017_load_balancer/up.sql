-- Load-balancer tables for the Unified Pentagonal Project
-- (load_balancer.rs): balancers, their backends, and per-backend
-- routing statistics. This migration is the sketch from
-- `src/modules/load_balancer.rs` (section 4 of the integrator
-- notes), applied verbatim except where noted below.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `load_balancers.tls_mode` is `off`, `terminate`, or
--     `passthrough`; `strategy` is `round_robin`,
--     `least_connections`, `weighted`, or `ip_hash`; `status` is
--     `active`, `degraded`, or `disabled`.
--   - `lb_backends.source_kind` is `provider`, `cluster`, or
--     `address`. For cluster backends `source_id` is
--     `"<cluster_id>/<node_id>"`.
--   - `lb_backend_stats` has `backend_id` as its primary key (one
--     stats row per backend). The sketch gives it only
--     `updated_at`; `created_at` is added here per the project
--     convention that every table carries both timestamps, and
--     normally equals the time the backend was first routed.
--   - Primary keys use `gen_random_uuid()` (the sketch writes
--     bare `UUID PRIMARY KEY`); the `pgcrypto` extension is
--     created for it, following migration 000002.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- A load balancer: a listen address and port, TLS mode, routing
-- strategy, and health-check configuration.
CREATE TABLE IF NOT EXISTS load_balancers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    listen_address VARCHAR NOT NULL,
    listen_port INTEGER NOT NULL CHECK (listen_port BETWEEN 1 AND 65535),
    tls_mode VARCHAR NOT NULL DEFAULT 'off' CHECK (tls_mode IN ('off', 'terminate', 'passthrough')),
    domain VARCHAR,
    certificate_name VARCHAR,
    strategy VARCHAR NOT NULL DEFAULT 'round_robin' CHECK (strategy IN ('round_robin', 'least_connections', 'weighted', 'ip_hash')),
    health_check JSONB NOT NULL DEFAULT '{}'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    status VARCHAR NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'degraded', 'disabled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (listen_address, listen_port)
);

CREATE INDEX IF NOT EXISTS idx_load_balancers_status
    ON load_balancers (status);

-- One backend of a balancer: a provider server, a cluster node, or
-- a plain address.
CREATE TABLE IF NOT EXISTS lb_backends (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    balancer_id UUID NOT NULL REFERENCES load_balancers (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR NOT NULL,
    source_kind VARCHAR NOT NULL DEFAULT 'address' CHECK (source_kind IN ('provider', 'cluster', 'address')),
    source_id VARCHAR,
    address VARCHAR NOT NULL,
    weight INTEGER NOT NULL DEFAULT 1 CHECK (weight BETWEEN 1 AND 100),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    draining BOOLEAN NOT NULL DEFAULT FALSE,
    health VARCHAR NOT NULL DEFAULT 'unknown' CHECK (health IN ('unknown', 'healthy', 'unhealthy')),
    last_check_at TIMESTAMPTZ,
    last_latency_ms BIGINT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    consecutive_successes INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_successes >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_lb_backends_balancer_id
    ON lb_backends (balancer_id);
CREATE INDEX IF NOT EXISTS idx_lb_backends_health
    ON lb_backends (health);

-- Per-backend routing statistics, keyed by backend.
CREATE TABLE IF NOT EXISTS lb_backend_stats (
    backend_id UUID PRIMARY KEY REFERENCES lb_backends (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    requests_routed BIGINT NOT NULL DEFAULT 0 CHECK (requests_routed >= 0),
    errors BIGINT NOT NULL DEFAULT 0 CHECK (errors >= 0),
    total_latency_ms BIGINT NOT NULL DEFAULT 0 CHECK (total_latency_ms >= 0),
    latency_samples BIGINT NOT NULL DEFAULT 0 CHECK (latency_samples >= 0),
    checks_total BIGINT NOT NULL DEFAULT 0 CHECK (checks_total >= 0),
    checks_ok BIGINT NOT NULL DEFAULT 0 CHECK (checks_ok >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
