-- Billing-panel tables for the Unified Pentagonal Project
-- (panels.rs): stored panel connections (WHMCS, cPanel / WHM,
-- DirectAdmin, Plesk, CyberPanel, generic) and the records synced
-- from them.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `panel_connections.kind` is `whmcs`, `cpanel`,
--     `directadmin`, `plesk`, `cyberpanel`, or `generic`.
--   - `credentials` stores the credential document as JSONB. The
--     values are write-only: they are never serialized into a view,
--     never logged, and never included in hook payloads. They must
--     be encrypted before insert, the same reservation as the
--     `*_encrypted` columns of `vcs_provider_settings` (no
--     encryption helper exists yet).
--   - `generic_config` is the JSON form of `GenericPanelConfig`
--     (auth type, endpoint paths, field mappings); it contains no
--     credentials.
--   - `panel_synced_records` is keyed by (connection_id,
--     collection, remote_id): one imported record per remote id,
--     with its two-way sync state. `collection` is one of
--     `clients`, `products`, `services`, `domains`, `invoices`,
--     or `server_accounts`. `direction` is `remote_to_local`,
--     `unchanged`, `conflict`, or `local_to_remote`. `payload` is
--     the last payload applied locally (the remote payload, unless
--     the local version won a conflict).
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One stored panel connection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS panel_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('whmcs', 'cpanel', 'directadmin', 'plesk', 'cyberpanel', 'generic')),
    endpoint VARCHAR,
    credentials JSONB NOT NULL DEFAULT '{}'::jsonb,
    generic_config JSONB,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_tested_at TIMESTAMPTZ,
    last_status VARCHAR,
    last_synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_panel_connections_name
    ON panel_connections (name);
CREATE INDEX IF NOT EXISTS idx_panel_connections_kind
    ON panel_connections (kind);
CREATE INDEX IF NOT EXISTS idx_panel_connections_enabled
    ON panel_connections (enabled);

-- ---------------------------------------------------------------------------
-- One record imported from a panel, with its two-way sync state.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS panel_synced_records (
    connection_id UUID NOT NULL REFERENCES panel_connections (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    collection VARCHAR NOT NULL CHECK (collection IN ('clients', 'products', 'services', 'domains', 'invoices', 'server_accounts')),
    remote_id VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    hash VARCHAR NOT NULL,
    last_synced_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    direction VARCHAR NOT NULL CHECK (direction IN ('remote_to_local', 'unchanged', 'conflict', 'local_to_remote')),
    conflict BOOLEAN NOT NULL DEFAULT FALSE,
    local_changed BOOLEAN NOT NULL DEFAULT FALSE,
    prefer_local BOOLEAN NOT NULL DEFAULT FALSE,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (connection_id, collection, remote_id)
);

CREATE INDEX IF NOT EXISTS idx_panel_synced_records_connection_id
    ON panel_synced_records (connection_id);
CREATE INDEX IF NOT EXISTS idx_panel_synced_records_collection
    ON panel_synced_records (collection);
CREATE INDEX IF NOT EXISTS idx_panel_synced_records_conflict
    ON panel_synced_records (conflict);
