-- Tenant isolation control-plane tables for the Unified Pentagonal Project
-- (`src/modules/tenants.rs`, batch 17 builder B2).
--
-- Migration number note: assigned 000093 when written, renumbered
-- to 000100 by the integration renumber (gapless 000001–000108);
-- the parallel 000093_product_subdomains claim was resolved by
-- the same renumber. Do not renumber.
--
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs). This migration has never been executed against a
-- live PostgreSQL database.
--
-- Scope: CONTROL PLANE ONLY. These tables live in the `public`
-- schema and describe tenants themselves (identity, lifecycle,
-- provisioning progress, module grants). Tenant business data lives
-- in one schema per tenant (see `crate::core::tenancy::schema_name`)
-- and is never stored here. The in-memory DashMap stores in
-- `src/modules/tenants.rs` are authoritative at runtime for this
-- round, following the licensing / SaaS-packages precedent; these
-- tables are the durable persistence targets and the integrator
-- wires them into `src/db/schema.rs` / `src/db/models.rs` from the
-- Diesel snippets in the module header of `src/modules/tenants.rs`.
--
-- Notes on the mapping:
--   - `tenants.id` is the tenant id itself: a uuidv7 generated
--     server-side (`Uuid::now_v7()`), never client-supplied. The
--     same value is the company id the rest of the platform keys
--     tenant-scoped records by, and the schema name is derived
--     from it (`tenant_<uuidv7 with hyphens as underscores>`).
--   - `tenants.slug` is the subdomain label, validated in code
--     (lowercase, 3-63 chars, reserved-label deny-list from the
--     tenant settings) and again here by CHECK, so a bad row can
--     never be inserted by another writer either.
--   - `tenants.status` is a closed vocabulary enforced by CHECK:
--     provisioning, active, suspended, deprovisioning, deleted,
--     provision_failed. The allowed transitions are enforced
--     server-side in `src/modules/tenants.rs`
--     (`transition_allowed`); the database only guards the
--     vocabulary, never the graph.
--   - `package_id`, `preset_id`, and `plan_id` are plain
--     references with NO foreign key on purpose: packages,
--     presets, and plans are owned by other modules and this
--     module must not assume their tables or migration order.
--     The same applies to `owner_user_id` (a user id owned by the
--     users module) and to `granted_via_id` in the grants table
--     (a license, subscription, or package id owned elsewhere).
--   - `tenant_subdomains` is owned by the saas_domains module and
--     is deliberately NOT recreated here.
--   - `tenant_provisioning_steps` is the idempotency record for
--     provisioning: re-running a half-finished provision resumes
--     at the first incomplete step instead of duplicating work.
--     One row per (tenant, step); the step names are the fixed
--     pipeline vocabulary (`create_record`, `create_schema`,
--     `run_migrations`, `register_subdomain`, `grant_modules`,
--     `activate`).
--   - `tenant_module_grants` records exactly which modules a
--     tenant was granted and through what (license, subscription,
--     package, preset, or owner entitlement). A module the tenant
--     is not entitled to is NEVER recorded here. Revocation is a
--     soft delete (`revoked_at`); the partial unique index keeps
--     at most one live grant per (tenant, module).
--   - No money is stored in any table here, and no secret is
--     stored either: `suspended_reason` / `error` are free text
--     supplied by an administrator or produced by the server and
--     must never contain a credential.
--   - Tenant data is NEVER deleted by this migration or by the
--     lifecycle flows that write these tables, except through the
--     dedicated destroy flow (builder B3, `tenant_destroy.rs`),
--     which exports first, requires confirmation, and audits.

CREATE TABLE tenants (
    id UUID PRIMARY KEY,
    slug VARCHAR(63) NOT NULL UNIQUE,
    display_name VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'provisioning'
        CHECK (status IN ('provisioning', 'active', 'suspended', 'deprovisioning', 'deleted', 'provision_failed')),
    owner_user_id UUID NOT NULL,
    package_id UUID,
    preset_id UUID,
    plan_id VARCHAR(128),
    subdomain VARCHAR(253),
    subdomain_pending BOOLEAN NOT NULL DEFAULT FALSE,
    schema_name VARCHAR(63) NOT NULL UNIQUE,
    suspended_at TIMESTAMPTZ,
    suspended_reason TEXT,
    destroyed_at TIMESTAMPTZ,
    export_reference TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (slug ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'),
    CHECK (schema_name ~ '^tenant_[0-9a-f]{32}$')
);
CREATE INDEX tenants_status_idx ON tenants (status);
CREATE INDEX tenants_owner_idx ON tenants (owner_user_id);
CREATE INDEX tenants_package_idx ON tenants (package_id);
CREATE INDEX tenants_created_idx ON tenants (created_at);

CREATE TABLE tenant_provisioning_steps (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    step VARCHAR(64) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
    attempt INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, step)
);
CREATE INDEX tenant_provisioning_steps_tenant_idx ON tenant_provisioning_steps (tenant_id);
CREATE INDEX tenant_provisioning_steps_status_idx ON tenant_provisioning_steps (tenant_id, status);

CREATE TABLE tenant_module_grants (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    module_id VARCHAR(128) NOT NULL,
    granted_via VARCHAR(64) NOT NULL
        CHECK (granted_via IN ('license', 'subscription', 'package', 'preset', 'owner_entitlement', 'builtin', 'manual_grant')),
    granted_via_id UUID,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    granted_by VARCHAR(255),
    revoked_at TIMESTAMPTZ,
    revoke_reason TEXT
);
CREATE INDEX tenant_module_grants_tenant_idx ON tenant_module_grants (tenant_id);
CREATE INDEX tenant_module_grants_module_idx ON tenant_module_grants (module_id);
CREATE INDEX tenant_module_grants_live_idx ON tenant_module_grants (tenant_id, module_id) WHERE revoked_at IS NULL;
