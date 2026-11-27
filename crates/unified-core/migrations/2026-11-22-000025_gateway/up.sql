-- Pingora gateway tables for the Unified Pentagonal Project
-- (`src/gateway/routing.rs`, `src/gateway/mod.rs`): routing rules
-- and the proxied-request log.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory / config-file rule store is
-- still authoritative; see TODO.md):
--   - `gateway_routing_rules.upstream` is the JSON form of
--     `UpstreamRef`: a balancer reference (`{kind: balancer,
--     balancer_id}`), one plain address (`{kind: address,
--     address}`), or a weighted address pool (`{kind:
--     address_pool, addresses: [{address, weight}]}`).
--     `headers` is the JSON array of header matchers; `health_check`
--     is the JSON health-check configuration for direct upstreams.
--     `compiled_path_regex` from the struct is NOT stored: it is a
--     compiled form, rebuilt by `compile_regex` after loading.
--   - `priority` decides the match order (higher wins; ties break
--     on `created_at`, oldest first). `strategy` is the gateway's
--     own selection strategy for the rule's pool.
--   - `rate_limit_override` names a rate-limit rule owned by the
--     hardening module; the gateway only stores the reference.
--   - `gateway_request_log` is the durable form of the in-memory
--     ring buffer of proxied requests. Method, host, path, status,
--     duration, and upstream only: request and response bodies are
--     never logged, and authorization headers are never read into
--     the record at all. `backend_id` is the load-balancer backend
--     that served the request, when one was selected; it is UUID
--     without a foreign key because the gateway must keep logging
--     even when the balancer row is gone (the log is append-only
--     evidence, not a live join).
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One gateway routing rule.
CREATE TABLE IF NOT EXISTS gateway_routing_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    priority INTEGER NOT NULL DEFAULT 100 CHECK (priority BETWEEN -10000 AND 10000),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    host VARCHAR,
    path_prefix VARCHAR,
    path_regex VARCHAR,
    headers JSONB NOT NULL DEFAULT '[]'::jsonb,
    upstream JSONB NOT NULL DEFAULT '{}'::jsonb,
    strategy VARCHAR NOT NULL,
    require_tls BOOLEAN NOT NULL DEFAULT FALSE,
    rate_limit_override VARCHAR,
    health_check JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_gateway_routing_rules_name
    ON gateway_routing_rules (name);
CREATE INDEX IF NOT EXISTS idx_gateway_routing_rules_enabled
    ON gateway_routing_rules (enabled);
CREATE INDEX IF NOT EXISTS idx_gateway_routing_rules_priority
    ON gateway_routing_rules (priority);

-- One proxied request, as logged by the gateway.
CREATE TABLE IF NOT EXISTS gateway_request_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id VARCHAR NOT NULL,
    method VARCHAR NOT NULL,
    host VARCHAR NOT NULL,
    path VARCHAR NOT NULL,
    status INTEGER NOT NULL CHECK (status >= 100 AND status < 600),
    duration_ms BIGINT NOT NULL CHECK (duration_ms >= 0),
    upstream VARCHAR,
    backend_id UUID,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_gateway_request_log_request_id
    ON gateway_request_log (request_id);
CREATE INDEX IF NOT EXISTS idx_gateway_request_log_recorded_at
    ON gateway_request_log (recorded_at);
CREATE INDEX IF NOT EXISTS idx_gateway_request_log_status
    ON gateway_request_log (status);
CREATE INDEX IF NOT EXISTS idx_gateway_request_log_backend_id
    ON gateway_request_log (backend_id);
