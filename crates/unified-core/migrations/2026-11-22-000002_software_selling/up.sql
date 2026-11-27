-- Software-selling tables for the Unified Pentagonal Project:
-- enterprise software manager (software.rs), storefront (store.rs),
-- and third-party VCS integrations (vcs.rs).
-- Matches src/db/schema.rs exactly. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md, "Software-Selling Round"):
--   - Money is DOUBLE PRECISION to mirror the f64 values used by the
--     in-memory store. NUMERIC would be the better long-term choice
--     for money; do not mix the two without a data migration.
--   - `software_releases` is not in the builders' table list, but it
--     is required: `release_files.release_id` needs a referential
--     target, and the software module's Release struct has no other
--     table.
--   - `software_versions.file_id` has no foreign key on purpose: the
--     reverse reference (`release_files.version_id`) already links
--     files to versions, and a second link would be circular.
--   - `vcs_provider_settings` stores tokens in `*_encrypted` columns.
--     No encryption helper exists yet, so the columns reserve the
--     shape; encrypting values before insert is still open work.
--   - Unique indexes that include a nullable column use COALESCE so
--     the uniqueness rule also applies when that part is NULL
--     (plain UNIQUE treats NULLs as distinct).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Hierarchical taxonomy: groups and categories (cycle-safe tree).
CREATE TABLE IF NOT EXISTS software_taxonomy (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind VARCHAR NOT NULL CHECK (kind IN ('group', 'category')),
    slug VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    parent_id UUID REFERENCES software_taxonomy (id) ON DELETE CASCADE,
    sort_order BIGINT NOT NULL DEFAULT 0,
    depth BIGINT NOT NULL DEFAULT 0,
    path VARCHAR NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_software_taxonomy_parent_id
    ON software_taxonomy (parent_id);
CREATE INDEX IF NOT EXISTS idx_software_taxonomy_kind
    ON software_taxonomy (kind);
-- One slug per (kind, parent); COALESCE so root nodes (NULL parent)
-- are covered by the rule as well.
CREATE UNIQUE INDEX IF NOT EXISTS uq_software_taxonomy_kind_parent_slug
    ON software_taxonomy (
        kind,
        COALESCE(parent_id, '00000000-0000-0000-0000-000000000000'::uuid),
        slug
    );

-- Software records: plugins, themes, software, libraries.
CREATE TABLE IF NOT EXISTS software (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug VARCHAR NOT NULL UNIQUE,
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('plugin', 'theme', 'software', 'library')),
    description TEXT NOT NULL DEFAULT '',
    vendor_company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    group_id UUID REFERENCES software_taxonomy (id) ON DELETE SET NULL,
    category_id UUID REFERENCES software_taxonomy (id) ON DELETE SET NULL,
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'deprecated', 'archived')),
    homepage_url VARCHAR,
    repository_url VARCHAR,
    documentation_url VARCHAR,
    screenshots JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_software_kind ON software (kind);
CREATE INDEX IF NOT EXISTS idx_software_status ON software (status);
CREATE INDEX IF NOT EXISTS idx_software_vendor_company_id ON software (vendor_company_id);
CREATE INDEX IF NOT EXISTS idx_software_group_id ON software (group_id);
CREATE INDEX IF NOT EXISTS idx_software_category_id ON software (category_id);

-- Software versions (semver) with platform requirements.
CREATE TABLE IF NOT EXISTS software_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product VARCHAR NOT NULL,
    version VARCHAR NOT NULL,
    changelog TEXT NOT NULL DEFAULT '',
    file_id UUID,
    license_required BOOLEAN NOT NULL DEFAULT FALSE,
    software_id UUID REFERENCES software (id) ON DELETE CASCADE,
    requirements JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_software_versions_software_id
    ON software_versions (software_id);
CREATE INDEX IF NOT EXISTS idx_software_versions_product
    ON software_versions (product);
-- One version string per software record.
CREATE UNIQUE INDEX IF NOT EXISTS uq_software_versions_software_version
    ON software_versions (software_id, version);

-- Releases: a version on a channel (stable / beta), draft or published.
CREATE TABLE IF NOT EXISTS software_releases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version_id UUID NOT NULL REFERENCES software_versions (id) ON DELETE CASCADE,
    channel VARCHAR NOT NULL CHECK (channel IN ('stable', 'beta')),
    notes TEXT NOT NULL DEFAULT '',
    status VARCHAR NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    download_count BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_software_releases_version_id
    ON software_releases (version_id);
CREATE INDEX IF NOT EXISTS idx_software_releases_status
    ON software_releases (status);

-- Release files with sha256 / sha512 checksums and integrity status.
CREATE TABLE IF NOT EXISTS release_files (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    release_id UUID NOT NULL REFERENCES software_releases (id) ON DELETE CASCADE,
    version_id UUID NOT NULL REFERENCES software_versions (id) ON DELETE CASCADE,
    filename VARCHAR NOT NULL,
    size_bytes BIGINT NOT NULL DEFAULT 0,
    checksum_sha256 VARCHAR,
    checksum_sha512 VARCHAR,
    download_url VARCHAR,
    storage_path VARCHAR,
    integrity_status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (integrity_status IN ('pending', 'verified', 'failed', 'unverified')),
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_release_files_release_id
    ON release_files (release_id);
CREATE INDEX IF NOT EXISTS idx_release_files_version_id
    ON release_files (version_id);
CREATE INDEX IF NOT EXISTS idx_release_files_integrity_status
    ON release_files (integrity_status);

-- Tags and their many-to-many assignment to software records.
CREATE TABLE IF NOT EXISTS software_tags (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug VARCHAR NOT NULL UNIQUE,
    name VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS software_tag_assignments (
    software_id UUID NOT NULL REFERENCES software (id) ON DELETE CASCADE,
    tag_id UUID NOT NULL REFERENCES software_tags (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (software_id, tag_id)
);

CREATE INDEX IF NOT EXISTS idx_software_tag_assignments_tag_id
    ON software_tag_assignments (tag_id);

-- Store products: sellable software with pricing model and vendor.
CREATE TABLE IF NOT EXISTS store_products (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    price DOUBLE PRECISION NOT NULL DEFAULT 0,
    currency VARCHAR NOT NULL DEFAULT 'USD',
    kind VARCHAR NOT NULL CHECK (kind IN ('theme', 'plugin', 'software')),
    license_product BOOLEAN NOT NULL DEFAULT FALSE,
    file_id UUID,
    slug VARCHAR UNIQUE,
    software_id UUID REFERENCES software (id) ON DELETE SET NULL,
    software_slug VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'published'
        CHECK (status IN ('draft', 'published')),
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    pricing_model VARCHAR NOT NULL DEFAULT 'one_time'
        CHECK (pricing_model IN ('one_time', 'subscription')),
    billing_period VARCHAR CHECK (billing_period IN ('monthly', 'yearly')),
    renewal_price DOUBLE PRECISION,
    trial_days INTEGER NOT NULL DEFAULT 0,
    updates_years INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_store_products_kind ON store_products (kind);
CREATE INDEX IF NOT EXISTS idx_store_products_status ON store_products (status);
CREATE INDEX IF NOT EXISTS idx_store_products_software_id
    ON store_products (software_id);
CREATE INDEX IF NOT EXISTS idx_store_products_software_slug
    ON store_products (software_slug);
CREATE INDEX IF NOT EXISTS idx_store_products_company_id
    ON store_products (company_id);

-- Pricing tiers / editions per product.
CREATE TABLE IF NOT EXISTS store_pricing_tiers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES store_products (id) ON DELETE CASCADE,
    slug VARCHAR NOT NULL,
    label VARCHAR NOT NULL,
    price DOUBLE PRECISION NOT NULL DEFAULT 0,
    renewal_price DOUBLE PRECISION,
    billing_period VARCHAR CHECK (billing_period IN ('monthly', 'yearly')),
    trial_days INTEGER NOT NULL DEFAULT 0,
    seats INTEGER NOT NULL DEFAULT 1,
    download_limit INTEGER NOT NULL DEFAULT 10,
    features JSONB NOT NULL DEFAULT '[]'::jsonb,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_store_pricing_tiers_product_id
    ON store_pricing_tiers (product_id);

-- Discount coupons.
CREATE TABLE IF NOT EXISTS store_coupons (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code VARCHAR NOT NULL UNIQUE,
    discount_type VARCHAR NOT NULL CHECK (discount_type IN ('percent', 'fixed')),
    discount_value DOUBLE PRECISION NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ,
    usage_limit INTEGER,
    used_count INTEGER NOT NULL DEFAULT 0,
    product_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_store_coupons_active ON store_coupons (active);

-- Orders with validated lifecycle and coupon totals.
CREATE TABLE IF NOT EXISTS store_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR NOT NULL,
    subtotal DOUBLE PRECISION NOT NULL DEFAULT 0,
    discount_total DOUBLE PRECISION NOT NULL DEFAULT 0,
    total DOUBLE PRECISION NOT NULL DEFAULT 0,
    currency VARCHAR NOT NULL DEFAULT 'USD',
    coupon_code VARCHAR,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    trial_until TIMESTAMPTZ,
    status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'payment_pending', 'paid', 'completed', 'refunded', 'cancelled')),
    provider VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_store_orders_user_id ON store_orders (user_id);
CREATE INDEX IF NOT EXISTS idx_store_orders_status ON store_orders (status);
CREATE INDEX IF NOT EXISTS idx_store_orders_company_id ON store_orders (company_id);
CREATE INDEX IF NOT EXISTS idx_store_orders_coupon_code ON store_orders (coupon_code);

CREATE TABLE IF NOT EXISTS store_order_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL REFERENCES store_orders (id) ON DELETE CASCADE,
    product_id UUID REFERENCES store_products (id) ON DELETE SET NULL,
    name VARCHAR NOT NULL,
    unit_price DOUBLE PRECISION NOT NULL DEFAULT 0,
    quantity INTEGER NOT NULL DEFAULT 1,
    line_total DOUBLE PRECISION NOT NULL DEFAULT 0,
    license_product BOOLEAN NOT NULL DEFAULT FALSE,
    file_id UUID,
    tier_id UUID REFERENCES store_pricing_tiers (id) ON DELETE SET NULL,
    tier_name VARCHAR,
    software_id UUID REFERENCES software (id) ON DELETE SET NULL,
    software_slug VARCHAR,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    pricing_model VARCHAR NOT NULL DEFAULT 'one_time',
    billing_period VARCHAR,
    seats INTEGER NOT NULL DEFAULT 1,
    expires_at TIMESTAMPTZ,
    download_limit INTEGER NOT NULL DEFAULT 10
);

CREATE INDEX IF NOT EXISTS idx_store_order_items_order_id
    ON store_order_items (order_id);
CREATE INDEX IF NOT EXISTS idx_store_order_items_product_id
    ON store_order_items (product_id);

-- Download entitlements and download history (mirror of issued licenses).
CREATE TABLE IF NOT EXISTS store_entitlements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    license_id UUID REFERENCES licenses (id) ON DELETE SET NULL,
    license_key VARCHAR,
    order_id UUID REFERENCES store_orders (id) ON DELETE SET NULL,
    user_id VARCHAR NOT NULL,
    product_id UUID REFERENCES store_products (id) ON DELETE SET NULL,
    tier_id UUID REFERENCES store_pricing_tiers (id) ON DELETE SET NULL,
    software_id UUID REFERENCES software (id) ON DELETE SET NULL,
    software_slug VARCHAR,
    release_id UUID REFERENCES software_releases (id) ON DELETE SET NULL,
    download_limit INTEGER NOT NULL DEFAULT 10,
    downloads_used INTEGER NOT NULL DEFAULT 0,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'expired', 'revoked')),
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_store_entitlements_license_id
    ON store_entitlements (license_id);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_order_id
    ON store_entitlements (order_id);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_user_id
    ON store_entitlements (user_id);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_status
    ON store_entitlements (status);

CREATE TABLE IF NOT EXISTS store_downloads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    license_id UUID REFERENCES licenses (id) ON DELETE SET NULL,
    release_id UUID REFERENCES software_releases (id) ON DELETE SET NULL,
    software_slug VARCHAR,
    order_id UUID REFERENCES store_orders (id) ON DELETE SET NULL,
    downloaded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_store_downloads_license_id
    ON store_downloads (license_id);
CREATE INDEX IF NOT EXISTS idx_store_downloads_release_id
    ON store_downloads (release_id);

-- Update-API catalog: latest known version per software slug.
CREATE TABLE IF NOT EXISTS store_catalog (
    slug VARCHAR PRIMARY KEY,
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('theme', 'plugin', 'software')),
    product_id UUID REFERENCES store_products (id) ON DELETE SET NULL,
    product_name VARCHAR NOT NULL,
    latest_version VARCHAR NOT NULL,
    release_id UUID REFERENCES software_releases (id) ON DELETE SET NULL,
    file_id UUID,
    changelog TEXT NOT NULL DEFAULT '',
    requires_wp VARCHAR,
    tested_wp VARCHAR,
    requires_php VARCHAR,
    license_required BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_store_catalog_product_id
    ON store_catalog (product_id);

-- Store mirror of licenses issued through `order.created`.
CREATE TABLE IF NOT EXISTS store_issued_licenses (
    license_id UUID PRIMARY KEY REFERENCES licenses (id) ON DELETE CASCADE,
    license_key VARCHAR NOT NULL,
    product VARCHAR NOT NULL,
    order_id UUID REFERENCES store_orders (id) ON DELETE SET NULL,
    user_id VARCHAR,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'suspended')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_store_issued_licenses_order_id
    ON store_issued_licenses (order_id);

-- VCS repository connections (GitHub / GitLab / Bitbucket).
CREATE TABLE IF NOT EXISTS vcs_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider VARCHAR NOT NULL
        CHECK (provider IN ('github', 'gitlab', 'bitbucket')),
    owner VARCHAR NOT NULL,
    repo VARCHAR NOT NULL,
    host VARCHAR,
    software_id UUID REFERENCES software (id) ON DELETE SET NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    sync_status VARCHAR NOT NULL DEFAULT 'never_synced'
        CHECK (sync_status IN ('never_synced', 'syncing', 'synced', 'failed')),
    last_synced_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_vcs_connections_provider
    ON vcs_connections (provider);
CREATE INDEX IF NOT EXISTS idx_vcs_connections_software_id
    ON vcs_connections (software_id);
CREATE INDEX IF NOT EXISTS idx_vcs_connections_sync_status
    ON vcs_connections (sync_status);
-- One connection per (provider, owner, repo, host); COALESCE so
-- cloud connections (NULL host) are covered by the rule as well.
CREATE UNIQUE INDEX IF NOT EXISTS uq_vcs_connections_provider_owner_repo_host
    ON vcs_connections (provider, owner, repo, COALESCE(host, ''));

-- VCS version drafts imported from a provider (never auto-published).
CREATE TABLE IF NOT EXISTS vcs_version_drafts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id UUID NOT NULL REFERENCES vcs_connections (id) ON DELETE CASCADE,
    software_id UUID REFERENCES software (id) ON DELETE SET NULL,
    provider VARCHAR NOT NULL
        CHECK (provider IN ('github', 'gitlab', 'bitbucket')),
    version VARCHAR NOT NULL,
    tag VARCHAR NOT NULL,
    title VARCHAR,
    changelog TEXT NOT NULL DEFAULT '',
    published_at TIMESTAMPTZ,
    status VARCHAR NOT NULL DEFAULT 'draft',
    source VARCHAR NOT NULL DEFAULT 'import'
        CHECK (source IN ('import', 'sync', 'webhook')),
    assets JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (connection_id, tag)
);

CREATE INDEX IF NOT EXISTS idx_vcs_version_drafts_connection_id
    ON vcs_version_drafts (connection_id);
CREATE INDEX IF NOT EXISTS idx_vcs_version_drafts_software_id
    ON vcs_version_drafts (software_id);
CREATE INDEX IF NOT EXISTS idx_vcs_version_drafts_status
    ON vcs_version_drafts (status);

-- VCS sync history (manual and webhook-triggered sync runs).
CREATE TABLE IF NOT EXISTS vcs_sync_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id UUID NOT NULL REFERENCES vcs_connections (id) ON DELETE CASCADE,
    provider VARCHAR NOT NULL
        CHECK (provider IN ('github', 'gitlab', 'bitbucket')),
    status VARCHAR NOT NULL CHECK (status IN ('completed', 'failed')),
    releases_found INTEGER NOT NULL DEFAULT 0,
    drafts_created INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    rate_limit_remaining INTEGER,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    duration_ms BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_vcs_sync_history_connection_id
    ON vcs_sync_history (connection_id);
CREATE INDEX IF NOT EXISTS idx_vcs_sync_history_status
    ON vcs_sync_history (status);

-- VCS provider settings. Tokens live in `*_encrypted` columns and must
-- be encrypted before insert; the encryption helper is still open
-- work (see TODO.md). The columns only reserve the shape.
CREATE TABLE IF NOT EXISTS vcs_provider_settings (
    provider VARCHAR PRIMARY KEY
        CHECK (provider IN ('github', 'gitlab', 'bitbucket')),
    token_encrypted TEXT,
    webhook_secret_encrypted TEXT,
    api_base_override VARCHAR,
    gitlab_hosts JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
