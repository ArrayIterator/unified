-- Resource planner and analytics tables for the Unified
-- Pentagonal Project (planner.rs, analytics.rs): capacity pools,
-- usage samples, allocation plans, and the analytics event mirrors
-- (sales, license snapshots, metrics, health samples) plus report
-- definitions, runs, and schedules.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `capacity_pools.provider_connection_id` and `.cluster_id`
--     are VARCHAR, not foreign keys: the planner struct keeps
--     them as strings (a provider connection id or a cluster id),
--     and the pool may draw from a manually defined set of servers
--     with neither set.
--   - `allocation_plans.company_id` is VARCHAR for the same
--     reason (the planner struct keeps it as a string). `status`
--     is `draft`, `approved`, `active`, or `expired`; a plan past
--     its end date no longer holds capacity.
--   - Resource bundles (CPU cores, RAM, disk, bandwidth) are
--     stored as four DOUBLE PRECISION columns, matching the
--     `Resources` struct. They are measurements, not money.
--   - `analytics_sales.total` is NUMERIC(19,4): decimal money,
--     never DOUBLE PRECISION (see migration 000003).
--     `analytics_sales.order_id` and `.company_id` are VARCHAR:
--     the analytics module mirrors store events, whose ids are
--     strings there.
--   - `analytics_license_snapshots` is keyed by `license_id`: one
--     mirror row per license, maintained from license events.
--   - `analytics_report_runs.rows` is the JSON array of result
--     rows; `analytics_reports.filters` is the JSON filter map.
--   - The analytics dashboard mirrors of tasks and pools
--     (`TaskMirror`, `PoolMirror` in analytics.rs) are NOT
--     persisted here: they are read mirrors of the tasks and
--     planner tables, rebuilt from task / planner events.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- A capacity pool: the total resources available from one provider
-- connection, one cluster, or a manually defined set of servers.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS capacity_pools (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    provider_connection_id VARCHAR,
    cluster_id VARCHAR,
    capacity_cpu_cores DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (capacity_cpu_cores >= 0),
    capacity_ram_gb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (capacity_ram_gb >= 0),
    capacity_disk_gb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (capacity_disk_gb >= 0),
    capacity_bandwidth_tb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (capacity_bandwidth_tb >= 0),
    notes TEXT,
    last_sync_at TIMESTAMPTZ,
    last_server_count BIGINT CHECK (last_server_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_capacity_pools_provider_connection_id
    ON capacity_pools (provider_connection_id);
CREATE INDEX IF NOT EXISTS idx_capacity_pools_cluster_id
    ON capacity_pools (cluster_id);

-- ---------------------------------------------------------------------------
-- One recorded utilisation sample for a pool.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS pool_usage_samples (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pool_id UUID NOT NULL REFERENCES capacity_pools (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    used_cpu_cores DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (used_cpu_cores >= 0),
    used_ram_gb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (used_ram_gb >= 0),
    used_disk_gb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (used_disk_gb >= 0),
    used_bandwidth_tb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (used_bandwidth_tb >= 0),
    source VARCHAR NOT NULL CHECK (source IN ('api', 'provider-sync', 'service-monitor')),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pool_usage_samples_pool_id
    ON pool_usage_samples (pool_id);
CREATE INDEX IF NOT EXISTS idx_pool_usage_samples_recorded_at
    ON pool_usage_samples (recorded_at);

-- ---------------------------------------------------------------------------
-- An allocation plan: a reservation of pool capacity for a project
-- or company over a time window.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS allocation_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pool_id UUID NOT NULL REFERENCES capacity_pools (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR NOT NULL,
    company_id VARCHAR,
    project_name VARCHAR,
    reserved_cpu_cores DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (reserved_cpu_cores >= 0),
    reserved_ram_gb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (reserved_ram_gb >= 0),
    reserved_disk_gb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (reserved_disk_gb >= 0),
    reserved_bandwidth_tb DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (reserved_bandwidth_tb >= 0),
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('draft', 'approved', 'active', 'expired')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS idx_allocation_plans_pool_id
    ON allocation_plans (pool_id);
CREATE INDEX IF NOT EXISTS idx_allocation_plans_status
    ON allocation_plans (status);
CREATE INDEX IF NOT EXISTS idx_allocation_plans_ends_at
    ON allocation_plans (ends_at);

-- ---------------------------------------------------------------------------
-- One recorded sale or refund. Sales come from
-- `store.order.completed` events; refunds from
-- `store.order.refunded` events; both can also be recorded
-- manually.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS analytics_sales (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id VARCHAR,
    kind VARCHAR NOT NULL CHECK (kind IN ('sale', 'refund')),
    total NUMERIC(19, 4) NOT NULL CHECK (total >= 0),
    currency VARCHAR NOT NULL,
    products JSONB NOT NULL DEFAULT '[]'::jsonb,
    company_id VARCHAR,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_analytics_sales_order_id
    ON analytics_sales (order_id);
CREATE INDEX IF NOT EXISTS idx_analytics_sales_kind
    ON analytics_sales (kind);
CREATE INDEX IF NOT EXISTS idx_analytics_sales_recorded_at
    ON analytics_sales (recorded_at);

-- ---------------------------------------------------------------------------
-- Mirror of one license, maintained from license events. The
-- dashboard and the license summary count from this mirror.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS analytics_license_snapshots (
    license_id VARCHAR PRIMARY KEY,
    license_key VARCHAR,
    product VARCHAR NOT NULL DEFAULT '',
    status VARCHAR NOT NULL CHECK (status IN ('active', 'suspended', 'revoked', 'expired')),
    last_event_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_analytics_license_snapshots_status
    ON analytics_license_snapshots (status);

-- ---------------------------------------------------------------------------
-- One recorded usage metric: an API call, a download, a login, or
-- an upload.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS analytics_metric_samples (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind VARCHAR NOT NULL CHECK (kind IN ('api_call', 'download', 'login', 'upload')),
    source VARCHAR,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_analytics_metric_samples_kind
    ON analytics_metric_samples (kind);
CREATE INDEX IF NOT EXISTS idx_analytics_metric_samples_recorded_at
    ON analytics_metric_samples (recorded_at);

-- ---------------------------------------------------------------------------
-- One recorded health observation for a node or service.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS analytics_health_samples (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    node_id VARCHAR NOT NULL,
    cluster_id VARCHAR,
    status VARCHAR NOT NULL CHECK (status IN ('healthy', 'degraded', 'unhealthy', 'unknown')),
    latency_ms BIGINT CHECK (latency_ms >= 0),
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_analytics_health_samples_node_id
    ON analytics_health_samples (node_id);
CREATE INDEX IF NOT EXISTS idx_analytics_health_samples_cluster_id
    ON analytics_health_samples (cluster_id);
CREATE INDEX IF NOT EXISTS idx_analytics_health_samples_checked_at
    ON analytics_health_samples (checked_at);

-- ---------------------------------------------------------------------------
-- A report definition: which metrics, over which range, grouped by
-- which dimensions.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS analytics_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    description TEXT,
    metrics JSONB NOT NULL DEFAULT '[]'::jsonb,
    dimensions JSONB NOT NULL DEFAULT '[]'::jsonb,
    range_preset VARCHAR,
    start TIMESTAMPTZ,
    "end" TIMESTAMPTZ,
    filters JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- One generated report run with its result rows.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS analytics_report_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    definition_id UUID NOT NULL REFERENCES analytics_reports (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    status VARCHAR NOT NULL CHECK (status IN ('completed', 'failed')),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    row_count BIGINT NOT NULL DEFAULT 0 CHECK (row_count >= 0),
    error TEXT,
    rows JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_analytics_report_runs_definition_id
    ON analytics_report_runs (definition_id);
CREATE INDEX IF NOT EXISTS idx_analytics_report_runs_status
    ON analytics_report_runs (status);

-- ---------------------------------------------------------------------------
-- A report definition on a schedule.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS analytics_scheduled_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    definition_id UUID NOT NULL REFERENCES analytics_reports (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    schedule VARCHAR NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    next_run_at TIMESTAMPTZ,
    last_run_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_analytics_scheduled_reports_definition_id
    ON analytics_scheduled_reports (definition_id);
CREATE INDEX IF NOT EXISTS idx_analytics_scheduled_reports_enabled
    ON analytics_scheduled_reports (enabled);
