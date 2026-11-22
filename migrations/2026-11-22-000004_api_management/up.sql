-- Public API management tables for the Unified Pentagonal Project:
-- versioned public API registry (api_manager.rs), API keys
-- (apikeys.rs), and the access-control rules + audit log that gate
-- public access (see the API Manager access-control design).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `api_versions.id` is a deliberate natural key (`v1`, `v2`, ...):
--     the version registry is keyed by that id in code.
--   - `api_prefix_settings` is a singleton row keyed by `key =
--     'global'`, the same pattern as `vcs_provider_settings`
--     (one row per provider).
--   - `api_keys` follows the richer `ApiKeyRecord` from apikeys.rs
--     (status lifecycle, rotation, rate tiers, usage counters). The
--     simpler `ApiKey` in api_manager.rs is a subset of this shape:
--     no status, no rotation, no tiers. One table serves both; the
--     reconciling builder maps the api-manager key onto this table.
--     Key hashes are SHA-256 hex digests. The api-manager key hash
--     (HMAC with a per-process random secret) cannot be verified
--     after a restart; only SHA-256-hashed keys are durable.
--   - `api_access_rules` default-deny: a public endpoint with no
--     rule is denied. `public_access` may only be set together with
--     `allowed` (enforced by CHECK below).
--   - `api_access_audit` records access-control *changes* (rule and
--     key administration), not every request decision.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Public API versions, each with its own settings.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_versions (
    id VARCHAR PRIMARY KEY,
    number INTEGER NOT NULL CHECK (number > 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    deprecated BOOLEAN NOT NULL DEFAULT FALSE,
    sunset_date TIMESTAMPTZ,
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 1000 CHECK (rate_limit_per_minute > 0),
    auth_required BOOLEAN NOT NULL DEFAULT TRUE,
    notes TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_api_versions_number
    ON api_versions (number);

-- ---------------------------------------------------------------------------
-- Custom endpoints registered through the API Manager, plus the
-- built-in endpoints the registry reports. `rules` is the JSON form
-- of the endpoint validation rules; `version_support` is the JSON
-- form of the VersionSupport declaration (all / min / max / range /
-- list). `rate_limit` 0 means "fall back to the owning version's
-- per-version limit".
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_endpoints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    method VARCHAR NOT NULL CHECK (method IN ('GET', 'POST', 'PUT', 'PATCH', 'DELETE', '*')),
    path VARCHAR NOT NULL,
    version VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    rate_limit INTEGER NOT NULL DEFAULT 0 CHECK (rate_limit >= 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    rules JSONB NOT NULL DEFAULT '[]'::jsonb,
    owner_module VARCHAR NOT NULL DEFAULT 'custom',
    auth_required BOOLEAN NOT NULL DEFAULT TRUE,
    source VARCHAR NOT NULL DEFAULT 'custom' CHECK (source IN ('custom', 'builtin')),
    version_support JSONB NOT NULL DEFAULT '{"kind": "all"}'::jsonb,
    execution_count BIGINT NOT NULL DEFAULT 0 CHECK (execution_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One custom endpoint per (method, path): a second endpoint with
-- the same method and path is a collision, whatever version it
-- claims to cover (see the collision check in api_manager.rs).
CREATE UNIQUE INDEX IF NOT EXISTS uq_api_endpoints_method_path
    ON api_endpoints (method, path);
CREATE INDEX IF NOT EXISTS idx_api_endpoints_owner_module
    ON api_endpoints (owner_module);
CREATE INDEX IF NOT EXISTS idx_api_endpoints_version
    ON api_endpoints (version);

-- ---------------------------------------------------------------------------
-- Global public API prefix setting (singleton row).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_prefix_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    prefix VARCHAR NOT NULL DEFAULT 'v',
    migrate_internal BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- API keys. The full key value is never stored: only the SHA-256 hex
-- digest (`key_hash`), the display prefix, and the last four
-- characters. Scopes are `module:action` strings (`*`,
-- `payments:*`, `store:read`, `billing:admin`); see
-- `key_has_scope` in apikeys.rs for the matching rules.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    owner_id UUID REFERENCES users (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    prefix VARCHAR NOT NULL,
    last4 VARCHAR NOT NULL,
    key_hash VARCHAR NOT NULL,
    scopes JSONB NOT NULL DEFAULT '[]'::jsonb,
    rate_tier VARCHAR NOT NULL DEFAULT 'standard' CHECK (rate_tier IN ('standard', 'elevated', 'high')),
    requests_per_minute INTEGER NOT NULL DEFAULT 60 CHECK (requests_per_minute > 0),
    status VARCHAR NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'rotating', 'revoked', 'expired')),
    expires_at TIMESTAMPTZ,
    grace_until TIMESTAMPTZ,
    rotated_from UUID REFERENCES api_keys (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    rotated_to UUID REFERENCES api_keys (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    usage_count BIGINT NOT NULL DEFAULT 0 CHECK (usage_count >= 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_api_keys_key_hash
    ON api_keys (key_hash);
CREATE INDEX IF NOT EXISTS idx_api_keys_owner_id
    ON api_keys (owner_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_status
    ON api_keys (status);

-- ---------------------------------------------------------------------------
-- Public-access rules. One rule per (method, path template, version);
-- `version` NULL means the rule applies to every version. A request
-- for a registered endpoint with no matching rule is denied by
-- default; a rule with `allowed = false` denies explicitly. An
-- endpoint is reachable without a key only when `public_access` is
-- true and the endpoint itself does not require authentication.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_access_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    method VARCHAR NOT NULL CHECK (method IN ('GET', 'POST', 'PUT', 'PATCH', 'DELETE', '*')),
    path VARCHAR NOT NULL,
    version VARCHAR,
    allowed BOOLEAN NOT NULL DEFAULT FALSE,
    public_access BOOLEAN NOT NULL DEFAULT FALSE,
    actor VARCHAR NOT NULL DEFAULT 'admin',
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (NOT public_access OR allowed)
);

-- COALESCE so the uniqueness rule also applies when `version` is
-- NULL (plain UNIQUE treats NULLs as distinct).
CREATE UNIQUE INDEX IF NOT EXISTS uq_api_access_rules_method_path_version
    ON api_access_rules (
        method,
        path,
        COALESCE(version, '*')
    );
CREATE INDEX IF NOT EXISTS idx_api_access_rules_path
    ON api_access_rules (path);

-- ---------------------------------------------------------------------------
-- Audit log of access-control changes: rule administration and key
-- administration (creation, revocation, scope and rate-limit
-- changes). Append-only; no update or delete path exists.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_access_audit (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor VARCHAR NOT NULL,
    action VARCHAR NOT NULL CHECK (action IN ('rule_created', 'rule_updated', 'rule_deleted', 'key_created', 'key_revoked', 'key_scopes_updated', 'key_rate_limit_updated')),
    target VARCHAR NOT NULL,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_api_access_audit_actor
    ON api_access_audit (actor);
CREATE INDEX IF NOT EXISTS idx_api_access_audit_action
    ON api_access_audit (action);
CREATE INDEX IF NOT EXISTS idx_api_access_audit_created_at
    ON api_access_audit (created_at);
