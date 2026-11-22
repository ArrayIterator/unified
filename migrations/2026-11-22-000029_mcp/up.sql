-- MCP server tables for the Unified Pentagonal Project
-- (`src/modules/mcp.rs`): one scoped, read-only MCP server record
-- per site, plus the module configuration.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - READ-ONLY, MVP (hard rule). The MCP server never writes. No
--     write-capable tool exists, and no MCP client input ever
--     becomes SQL: clients supply only values (bound parameters)
--     and the names of allowlisted tools, resources, and prompts.
--     Nothing in this migration changes that guarantee; these
--     tables only persist the server records and the module config.
--   - `mcp_servers.owner_id` references `users` (an API key whose
--     `owner_id` is set may only use servers owned by that user).
--     `company_id` / `site_id` reference `companies` /
--     `company_sites` with ON DELETE SET NULL.
--   - `mcp_servers.product_id` is a plain UUID reference with NO
--     foreign key: a server is sold as a product, and entitlement
--     is checked through the `product.entitlement.check` hook
--     (fail-closed), never by a join from this module. NULL means
--     the server was created without a product link
--     (platform-owned, not sold through the store).
--   - `mcp_servers.status` is `active` or `disabled`; disabled
--     servers answer 403. `allowed_tools` is the per-server tool
--     allowlist (empty means every built-in tool the key is scoped
--     to). `rate_limit_per_minute` is validated to 1..100000 in the
--     module (`validate_rate_limit` in mcp.rs); the key's own tier
--     still applies on top, the lower of the two wins.
--     `read_only_role` is the optional PostgreSQL role the
--     read-only queries run as (`SET LOCAL ROLE`), validated as a
--     bare identifier by the module.
--   - `mcp_settings` is the single-row settings table for the
--     module configuration (`McpConfig`: `read_only_role` and
--     `default_rate_limit_per_minute`, default 60), following the
--     `key VARCHAR PRIMARY KEY CHECK (key = 'global')` pattern of
--     `certificate_settings`, `logs_settings`, and
--     `cloudflare_settings`.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One scoped MCP server: the sellable unit. A customer buys "MCP for
-- my site" and gets one of these, mounted at
-- `/api/v1/mcp/servers/{id or slug}/mcp`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS mcp_servers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    owner_id UUID NOT NULL REFERENCES users (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    company_id UUID REFERENCES companies (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    site_id UUID REFERENCES company_sites (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    product_id UUID,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    is_public BOOLEAN NOT NULL DEFAULT FALSE,
    allowed_tools JSONB NOT NULL DEFAULT '[]'::jsonb,
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 60
        CHECK (rate_limit_per_minute >= 1 AND rate_limit_per_minute <= 100000),
    read_only_role VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_mcp_servers_slug
    ON mcp_servers (slug);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_owner_id
    ON mcp_servers (owner_id);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_company_id
    ON mcp_servers (company_id);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_site_id
    ON mcp_servers (site_id);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_product_id
    ON mcp_servers (product_id);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_status
    ON mcp_servers (status);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_is_public
    ON mcp_servers (is_public);

-- ---------------------------------------------------------------------------
-- MCP module configuration (single row, key 'global').
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS mcp_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    read_only_role VARCHAR,
    default_rate_limit_per_minute INTEGER NOT NULL DEFAULT 60
        CHECK (default_rate_limit_per_minute >= 1 AND default_rate_limit_per_minute <= 100000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
