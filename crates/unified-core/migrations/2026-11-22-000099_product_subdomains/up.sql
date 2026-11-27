-- Per-product subdomain tables for the Unified Pentagonal Project
-- (`src/modules/saas_domains.rs`, batch 15 builder B2): each
-- purchased product or package claims its own subdomain
-- (`<label>.<wildcard_base>`), scoped to that product.
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
--     removes its product subdomains and their event log.
--   - `site_id` is a plain Uuid with NO foreign key on purpose: a
--     site id may be minted by a provisioning handoff before the
--     SaaS module has created the site row, and the SaaS module
--     stays the source of truth for sites.
--   - `claimed_by` / `actor` are TEXT actor ids (the `X-User-Id`
--     header), not necessarily the UUID primary key of the `users`
--     table, matching the convention used by other modules for
--     actor references.
--   - `product_id` / `package_id` are opaque TEXT identifiers
--     (products-module product ids, or sellable module ids such as
--     `academic` and `exams`): no foreign key, because the products
--     and packages modules own those identifiers and this table
--     must not assume their tables or migration order. The module
--     validates the identifier shape (1-128 chars, lowercase
--     letters, digits, hyphens, underscores).
--   - `custom_domain` holds a verified whitelabel domain that
--     replaces the subdomain for the same product scope. It is only
--     ever set from the `whitelabel.domain.verified` event, never
--     from an API request, and the module only serves it once
--     verified (presence in this column implies verified).
--   - Statuses are TEXT with CHECK constraints, not enums: the
--     allowed statuses are the module's lifecycle vocabulary
--     (`pending_verification`, `active`, `suspended`, `released`).
--   - Uniqueness is enforced in two layers. The database enforces
--     what a partial unique index can express: one live host per
--     label (`uq_saas_product_subdomains_host_live`) and one live
--     record per (tenant, product, package)
--     (`uq_saas_product_subdomains_tenant_product_live`, split in
--     two because `package_id` is nullable). The release grace
--     period is NOT expressible as an index: a `released` record
--     still blocks its label while `grace_until` is in the future,
--     and frees it once the grace period ends. That rule lives in
--     the module's availability check (`product_host_blocked`),
--     computed from `grace_until`; the index deliberately excludes
--     `released` rows so the database never contradicts it.
--   - `saas_product_subdomain_events` is the append-only audit
--     trail for lifecycle transitions (claimed, verified,
--     activated, suspended, release_started, released). The
--     module emits the same transitions on the hook bus; the audit
--     module records them once the event names are added to its
--     audited list (see docs/integration/INTEGRATION_NOTES_BATCH15_B2.md).
--   - Migration number: 000099 after the integration renumber
--     (000093 when written; the pre-renumber duplicates - 000090
--     twice, 000100_calculator out of sequence - were resolved by
--     the gapless renumber, see docs/integration/INTEGRATION_NOTES_INTEGRATION2.md sec. 4).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

CREATE TABLE saas_product_subdomains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    product_id VARCHAR(128) NOT NULL CHECK (char_length(product_id) BETWEEN 1 AND 128),
    package_id VARCHAR(128) CHECK (package_id IS NULL OR char_length(package_id) BETWEEN 1 AND 128),
    label VARCHAR(63) NOT NULL,
    host VARCHAR(253) NOT NULL,
    custom_domain VARCHAR(253),
    site_id UUID,
    status VARCHAR(32) NOT NULL DEFAULT 'pending_verification'
        CHECK (status IN ('pending_verification', 'active', 'suspended', 'released')),
    claimed_by TEXT NOT NULL,
    claimed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    verified_at TIMESTAMPTZ,
    released_at TIMESTAMPTZ,
    grace_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE saas_product_subdomain_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_subdomain_id UUID NOT NULL REFERENCES saas_product_subdomains (id) ON DELETE CASCADE,
    event VARCHAR(64) NOT NULL CHECK (char_length(event) BETWEEN 1 AND 64),
    actor TEXT NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One live host per label (a label is globally unique while live).
CREATE UNIQUE INDEX uq_saas_product_subdomains_host_live
    ON saas_product_subdomains (host)
    WHERE status IN ('pending_verification', 'active', 'suspended');

-- One live record per (tenant, product, package) when a package is named.
CREATE UNIQUE INDEX uq_saas_product_subdomains_tenant_product_package_live
    ON saas_product_subdomains (company_id, product_id, package_id)
    WHERE status IN ('pending_verification', 'active', 'suspended')
      AND package_id IS NOT NULL;

-- One live record per (tenant, product) for standalone product claims.
CREATE UNIQUE INDEX uq_saas_product_subdomains_tenant_product_live
    ON saas_product_subdomains (company_id, product_id)
    WHERE status IN ('pending_verification', 'active', 'suspended')
      AND package_id IS NULL;

CREATE INDEX idx_saas_product_subdomains_company_id
    ON saas_product_subdomains (company_id);
CREATE INDEX idx_saas_product_subdomains_status
    ON saas_product_subdomains (status);
CREATE INDEX idx_saas_product_subdomains_product_id
    ON saas_product_subdomains (product_id);
CREATE INDEX idx_saas_product_subdomains_label
    ON saas_product_subdomains (label);
CREATE INDEX idx_saas_product_subdomains_custom_domain
    ON saas_product_subdomains (custom_domain);
CREATE INDEX idx_saas_product_subdomains_grace_until
    ON saas_product_subdomains (grace_until);

CREATE INDEX idx_saas_product_subdomain_events_product_subdomain_id
    ON saas_product_subdomain_events (product_subdomain_id);
CREATE INDEX idx_saas_product_subdomain_events_event
    ON saas_product_subdomain_events (event);
