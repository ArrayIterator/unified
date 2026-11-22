-- Cloud / VPS / container provider tables for the Unified
-- Pentagonal Project (providers.rs): stored provider connections
-- and a cache of the servers each connection reports.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `provider_connections.kind` is one of `hetzner`, `ovh`,
--     `tencent`, `huawei`, `aws`, `google`, `virtualizor`,
--     `docker`, `kubernetes`, `redshift`, `digitalocean`, `linode`,
--     `openstack`, `proxmox`, `kvm`, `xen`, or `custom` (see
--     `ProviderKind`).
--   - `credentials` stores the credential document as JSONB. The
--     values are write-only: they are never serialized into a view,
--     never logged, and never included in hook payloads. They must
--     be encrypted before insert, the same reservation as the
--     `*_encrypted` columns of `vcs_provider_settings` (no
--     encryption helper exists yet).
--   - `custom_config` is the JSON form of `CustomProviderConfig`
--     (field mappings, power actions, endpoint paths); it contains
--     no credentials.
--   - `provider_servers` is a cache of remote inventory, not the
--     source of truth: rows are replaced on every inventory fetch.
--     `remote_id` is the provider-native server id; `fetched_at`
--     records when the row was last refreshed.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One stored provider connection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS provider_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('hetzner', 'ovh', 'tencent', 'huawei', 'aws', 'google', 'virtualizor', 'docker', 'kubernetes', 'redshift', 'digitalocean', 'linode', 'openstack', 'proxmox', 'kvm', 'xen', 'custom')),
    endpoint VARCHAR,
    region VARCHAR,
    project_id VARCHAR,
    credentials JSONB NOT NULL DEFAULT '{}'::jsonb,
    custom_config JSONB,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_tested_at TIMESTAMPTZ,
    last_status VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_provider_connections_name
    ON provider_connections (name);
CREATE INDEX IF NOT EXISTS idx_provider_connections_kind
    ON provider_connections (kind);
CREATE INDEX IF NOT EXISTS idx_provider_connections_enabled
    ON provider_connections (enabled);

-- ---------------------------------------------------------------------------
-- Cached inventory: one server, container, pod, deployment, or
-- cluster reported by a provider connection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS provider_servers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id UUID NOT NULL REFERENCES provider_connections (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    remote_id VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    item_kind VARCHAR NOT NULL CHECK (item_kind IN ('server', 'container', 'pod', 'deployment', 'cluster')),
    status VARCHAR NOT NULL CHECK (status IN ('running', 'stopped', 'starting', 'stopping', 'creating', 'error', 'unknown')),
    raw_status VARCHAR,
    region VARCHAR,
    public_ip VARCHAR,
    private_ip VARCHAR,
    cpu_cores INTEGER CHECK (cpu_cores >= 0),
    memory_mb BIGINT CHECK (memory_mb >= 0),
    disk_gb BIGINT CHECK (disk_gb >= 0),
    image VARCHAR,
    remote_created_at VARCHAR,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_provider_servers_connection_remote
    ON provider_servers (connection_id, remote_id);
CREATE INDEX IF NOT EXISTS idx_provider_servers_connection_id
    ON provider_servers (connection_id);
CREATE INDEX IF NOT EXISTS idx_provider_servers_status
    ON provider_servers (status);
