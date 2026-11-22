-- SaaS Domain Management tables for the Unified Pentagonal Project
-- (`src/modules/saas_domains.rs`): preregister domain reservations,
-- custom domain aliases, and tenant subdomain allocations.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the module header):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz. No BIGINT
--     anywhere.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE: deleting a company
--     removes its reservations, aliases, and subdomain allocations.
--   - `site_id` columns are plain Uuid with NO foreign key on
--     purpose: a site id may be minted by the reservation handoff
--     before the SaaS module has created the site row, and the SaaS
--     module stays the source of truth for sites.
--   - `reserved_by` / `approved_by` are TEXT actor ids (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table, matching the convention used by other
--     modules for actor references.
--   - Statuses and alias target types are TEXT with CHECK
--     constraints, not enums: the allowed alias target types are a
--     configurable module setting
--     (`saas_domains.alias_target_types`), so the database must not
--     hard-code the list. The CHECK on `target_type` only rejects
--     empty values; the module validates membership.
--   - Uniqueness is deliberately NOT enforced by the database:
--     one domain maps to at most one tenant across reservations,
--     aliases, allocations, and the `domains` table, and that
--     cross-table rule is enforced by the module's conflict check
--     (`domain_claimed`), which a partial unique index could not
--     express. The indexes below keep the conflict and resolution
--     lookups fast.
--   - Migration number: 000072 after the integration renumber
--     (000070 when written; the pre-renumber collisions — 000068
--     twice, 000100 out of sequence — were resolved by the
--     gapless renumber, see docs/integration/INTEGRATION_NOTES_INTEGRATION2.md §4).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

CREATE TABLE saas_domain_reservations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain VARCHAR(253) NOT NULL,
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    reserved_by TEXT NOT NULL,
    reserved_until TIMESTAMPTZ NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'reserved'
        CHECK (status IN ('reserved', 'active', 'released', 'expired')),
    site_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE saas_domain_aliases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain VARCHAR(253) NOT NULL,
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    target_type VARCHAR(32) NOT NULL CHECK (target_type <> ''),
    target_value VARCHAR(253) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending_verification'
        CHECK (status IN ('pending_verification', 'verified', 'suspended', 'released')),
    site_id UUID,
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE saas_subdomain_allocations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    label VARCHAR(63) NOT NULL,
    hostname VARCHAR(253) NOT NULL,
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    site_id UUID,
    status VARCHAR(32) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'pending_approval', 'suspended', 'released', 'rejected')),
    approved_by TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_saas_domain_reservations_domain
    ON saas_domain_reservations (domain);
CREATE INDEX idx_saas_domain_reservations_company_id
    ON saas_domain_reservations (company_id);
CREATE INDEX idx_saas_domain_reservations_status
    ON saas_domain_reservations (status);
CREATE INDEX idx_saas_domain_reservations_reserved_until
    ON saas_domain_reservations (reserved_until);

CREATE INDEX idx_saas_domain_aliases_domain
    ON saas_domain_aliases (domain);
CREATE INDEX idx_saas_domain_aliases_company_id
    ON saas_domain_aliases (company_id);
CREATE INDEX idx_saas_domain_aliases_status
    ON saas_domain_aliases (status);

CREATE INDEX idx_saas_subdomain_allocations_hostname
    ON saas_subdomain_allocations (hostname);
CREATE INDEX idx_saas_subdomain_allocations_company_id
    ON saas_subdomain_allocations (company_id);
CREATE INDEX idx_saas_subdomain_allocations_status
    ON saas_subdomain_allocations (status);
CREATE INDEX idx_saas_subdomain_allocations_label
    ON saas_subdomain_allocations (label);
