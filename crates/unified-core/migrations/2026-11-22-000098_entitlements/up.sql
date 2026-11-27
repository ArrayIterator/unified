-- Per-product entitlement tables for the Unified Pentagonal Project
-- (`src/modules/entitlements.rs`): an entitlement is one grant of one
-- module to one tenant (company) from one admin-defined source
-- (license, subscription, SaaS package, or manual grant). The
-- effective entitlement for a module is the union of its active
-- sources: any active source grants the module. No entitlement
-- record means not entitled (fail closed).
--
-- Migration number note: written as 000092 when
-- `2026-11-22-000091_library` appeared on disk; renumbered to
-- 000098 by the integration renumber (gapless 000001-000108).
-- Do not renumber.
--
-- TENANT-SCHEMA TABLES (standing rule: tenant-scoped data
-- access). These tables hold per-tenant business data, so they are
-- created PER TENANT SCHEMA by the migration runner with the
-- search_path set to that tenant's schema (derived from the tenant
-- namespace id, uuidv7, by the batch-17 schema_name helper) -
-- never in the `public` schema. Control-plane tables (tenants,
-- subscriptions, domains, billing, licensing) stay in `public`.
-- Every table carries `tenant_id` (the uuidv7 namespace id)
-- alongside `company_id` (the tenant's company key) so a row never
-- has to be re-keyed when it moves between schemas. Until the
-- batch-17 runner plumbing lands, the integrator applies this
-- migration per tenant schema manually; see
-- docs/integration/INTEGRATION_NOTES_BATCH15_B1.md for the convergence point.
--
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs). The migration has never been executed against a
-- live PostgreSQL database.
--
-- Notes on the mapping (the in-memory DashMap store is authoritative
-- for this round, following the licensing / SaaS-packages precedent;
-- these tables are the durable persistence targets and the
-- integrator wires them into `src/db/schema.rs` / `src/db/models.rs`
-- from the snippets in docs/integration/INTEGRATION_NOTES_BATCH15_B1.md):
--   - `company_id` is plain TEXT with NO foreign key on purpose:
--     tenants/companies live in the control plane and this module
--     must not assume their table or migration order. The same
--     applies to `source_id` (a license id, subscription id,
--     package id, or grant reference owned by another module) and
--     to `module_id` (a registered module id, validated against
--     the live module registry at write time, never by the DB).
--   - `source_kind` is a closed vocabulary: license, subscription,
--     saas_package, manual_grant. The kinds are the fixed taxonomy;
--     the sources themselves are admin-defined records, never code.
--   - `status` is a closed vocabulary: active, grace, revoked,
--     expired, disabled. Grace is the read-only period after a
--     lapse; a module in grace keeps read access (data export
--     still allowed) and rejects writes.
--   - Tenant data is NEVER deleted by the downgrade/expiry flow:
--     no table here is ever truncated by that flow, and the flow
--     issues no DELETE against tenant data tables at all. Only an
--     administrator may delete a manual-grant row, and the history
--     rows that describe it are kept.
--   - No secret is stored in any table here. `reason` is free text
--     supplied by an administrator; it must never contain a
--     credential.

CREATE TABLE entitlements (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    company_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('license', 'subscription', 'saas_package', 'manual_grant', 'owner_entitlement')),
    source_id TEXT NOT NULL,
    source_ref TEXT,
    module_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'grace', 'revoked', 'expired', 'disabled')),
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    granted_by TEXT,
    reason TEXT,
    grace_started_at TIMESTAMPTZ,
    grace_ends_at TIMESTAMPTZ,
    lapsed_at TIMESTAMPTZ,
    lapse_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX entitlements_tenant_idx ON entitlements (tenant_id);
CREATE INDEX entitlements_company_idx ON entitlements (company_id);
CREATE INDEX entitlements_module_idx ON entitlements (module_id);
CREATE INDEX entitlements_company_module_idx ON entitlements (company_id, module_id);
CREATE INDEX entitlements_source_idx ON entitlements (source_kind, source_id);
CREATE INDEX entitlements_status_idx ON entitlements (status);
CREATE INDEX entitlements_expires_idx ON entitlements (expires_at);

CREATE TABLE entitlement_history (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    entitlement_id UUID,
    company_id TEXT NOT NULL,
    module_id TEXT NOT NULL,
    action TEXT NOT NULL,
    actor_id TEXT,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX entitlement_history_tenant_idx ON entitlement_history (tenant_id);
CREATE INDEX entitlement_history_company_idx ON entitlement_history (company_id);
CREATE INDEX entitlement_history_module_idx ON entitlement_history (module_id);
CREATE INDEX entitlement_history_entitlement_idx ON entitlement_history (entitlement_id);
CREATE INDEX entitlement_history_created_idx ON entitlement_history (created_at);

CREATE TABLE entitlement_denied_attempts (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    company_id TEXT NOT NULL,
    module_id TEXT NOT NULL,
    actor_id TEXT,
    reason TEXT NOT NULL,
    error_code TEXT NOT NULL DEFAULT 'entitlement.required',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX entitlement_denied_attempts_tenant_idx ON entitlement_denied_attempts (tenant_id);
CREATE INDEX entitlement_denied_attempts_company_idx ON entitlement_denied_attempts (company_id);
CREATE INDEX entitlement_denied_attempts_module_idx ON entitlement_denied_attempts (module_id);
CREATE INDEX entitlement_denied_attempts_created_idx ON entitlement_denied_attempts (created_at);

CREATE TABLE entitlement_package_changes (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    company_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('license', 'subscription', 'saas_package', 'manual_grant', 'owner_entitlement')),
    source_id TEXT NOT NULL,
    new_module_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    newly_entitled JSONB NOT NULL DEFAULT '[]'::jsonb,
    kept JSONB NOT NULL DEFAULT '[]'::jsonb,
    kept_via_other_source JSONB NOT NULL DEFAULT '[]'::jsonb,
    requires_confirmation JSONB NOT NULL DEFAULT '[]'::jsonb,
    disabled_module_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    status TEXT NOT NULL DEFAULT 'pending_confirmation' CHECK (status IN ('pending_confirmation', 'applied', 'confirmed', 'discarded')),
    requested_by TEXT,
    decided_by TEXT,
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX entitlement_package_changes_tenant_idx ON entitlement_package_changes (tenant_id);
CREATE INDEX entitlement_package_changes_company_idx ON entitlement_package_changes (company_id);
CREATE INDEX entitlement_package_changes_status_idx ON entitlement_package_changes (status);

CREATE TABLE entitlement_settings (
    id TEXT PRIMARY KEY,
    grace_period_days BIGINT NOT NULL DEFAULT 14 CHECK (grace_period_days >= 0 AND grace_period_days <= 365),
    read_only_grace_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    default_deny BOOLEAN NOT NULL DEFAULT TRUE,
    updated_by TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE entitlement_module_activations (
    tenant_id TEXT NOT NULL,
    company_id TEXT NOT NULL,
    module_id TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    enabled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    enabled_by TEXT,
    source_kind TEXT,
    source_id TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (company_id, module_id)
);
CREATE INDEX entitlement_module_activations_module_idx ON entitlement_module_activations (module_id);
