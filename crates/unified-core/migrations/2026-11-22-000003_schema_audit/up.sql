-- Schema audit fixes for the Unified Pentagonal Project.
-- Matches src/db/schema.rs exactly. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- What this migration fixes (see docs/SCHEMA.md for the full audit):
--   1. Missing created_at / updated_at columns on eight tables.
--   2. Money columns stored as DOUBLE PRECISION become NUMERIC(19,4).
--      DOUBLE PRECISION is binary floating point and cannot represent
--      decimal money exactly; NUMERIC(19,4) is the project rule for money.
--   3. CHECK constraints for status / enum columns that had none, and for
--      value ranges the application code already enforces (currency shape,
--      country code shape, non-negative counters and amounts).
--   4. Missing indexes on foreign keys and on filtered / searched columns.
--   5. Missing unique constraints where the domain requires them.
--
-- Deliberately NOT changed here (documented in docs/SCHEMA.md):
--   - store_orders / store_entitlements / store_issued_licenses user_id
--     stays VARCHAR: the store module uses string user ids, so changing
--     the column type to UUID would break it. A UUID foreign key to
--     users(id) is a follow-up for the store module.
--   - file_id columns (software_versions, store_products,
--     store_order_items, store_catalog) get no foreign key: the file
--     identity is unresolved (see the 000002 migration notes).
--   - No partial unique index on company_sites (company_id) WHERE
--     is_primary: company.rs inserts a new primary site first and
--     demotes the old one afterwards, so the index would reject the
--     insert. The demote-before-insert reorder is a follow-up.
--   - No unique index on release_files (release_id, filename):
--     re-uploading a file with the same name would be rejected while
--     the in-memory store accepts it. Needs a replace-on-re-upload
--     decision in the software module first.
--   - No CHECK on vcs_version_drafts.status: only 'draft' is ever
--     written today and the later lifecycle states are undefined.
--   - Existing foreign keys keep the default ON UPDATE NO ACTION.
--     Their constraints were created inline, so their names are
--     generated; rewriting them needs constraint-name lookups. UUID
--     primary keys are never updated, so the default is safe.
--   - store_catalog keeps its natural VARCHAR primary key (slug) and
--     vcs_provider_settings keeps provider as its primary key; both
--     are deliberate natural keys, not surrogate-UUID tables.

-- ---------------------------------------------------------------------------
-- 1. Missing created_at / updated_at columns.
--    NOT NULL DEFAULT now() backfills existing rows with the migration
--    time, which is the honest value for a pre-launch database.
-- ---------------------------------------------------------------------------
ALTER TABLE software_releases
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE store_order_items
    ADD COLUMN created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE store_downloads
    ADD COLUMN created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE store_issued_licenses
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE vcs_sync_history
    ADD COLUMN created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- Junction / event rows are insert/delete-only in practice, but the
-- project convention is created_at + updated_at on every table, so the
-- columns are added here as well; updated_at normally equals created_at.
ALTER TABLE user_roles
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE software_tag_assignments
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE license_activations
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- ---------------------------------------------------------------------------
-- 2. Money: DOUBLE PRECISION -> NUMERIC(19,4).
--    USING (col::numeric(19,4)) converts the stored binary value to its
--    decimal representation; on a pre-launch database the values are
--    short decimals, so no rounding surprises are expected. Existing
--    DOUBLE values with more than 4 fractional digits would be rounded
--    by the cast; see docs/SCHEMA.md.
-- ---------------------------------------------------------------------------
ALTER TABLE store_products
    ALTER COLUMN price TYPE NUMERIC(19,4) USING price::numeric(19,4),
    ALTER COLUMN renewal_price TYPE NUMERIC(19,4) USING renewal_price::numeric(19,4);

ALTER TABLE store_pricing_tiers
    ALTER COLUMN price TYPE NUMERIC(19,4) USING price::numeric(19,4),
    ALTER COLUMN renewal_price TYPE NUMERIC(19,4) USING renewal_price::numeric(19,4);

ALTER TABLE store_coupons
    ALTER COLUMN discount_value TYPE NUMERIC(19,4) USING discount_value::numeric(19,4);

ALTER TABLE store_orders
    ALTER COLUMN subtotal TYPE NUMERIC(19,4) USING subtotal::numeric(19,4),
    ALTER COLUMN discount_total TYPE NUMERIC(19,4) USING discount_total::numeric(19,4),
    ALTER COLUMN total TYPE NUMERIC(19,4) USING total::numeric(19,4);

ALTER TABLE store_order_items
    ALTER COLUMN unit_price TYPE NUMERIC(19,4) USING unit_price::numeric(19,4),
    ALTER COLUMN line_total TYPE NUMERIC(19,4) USING line_total::numeric(19,4);

-- ---------------------------------------------------------------------------
-- 3. CHECK constraints.
--    Every value list below is taken from the validation code in
--    src/modules (licensing.rs, domains.rs, geo.rs, store.rs), not
--    invented here; see docs/SCHEMA.md for the source of each list.
-- ---------------------------------------------------------------------------

-- Licenses: the four states validated in licensing.rs.
ALTER TABLE licenses
    ADD CONSTRAINT chk_licenses_status
        CHECK (status IN ('active', 'suspended', 'revoked', 'expired')),
    ADD CONSTRAINT chk_licenses_max_activations
        CHECK (max_activations >= 1);

-- Domains: SSL states used by domains.rs / hosting.rs / saas.rs.
-- 'failed' is reserved for the SSL renewal flow; it is not written by
-- any module today but is the natural failure state of that flow.
ALTER TABLE domains
    ADD CONSTRAINT chk_domains_ssl_status
        CHECK (ssl_status IN ('none', 'pending', 'active', 'expired', 'renewing', 'failed'));

-- Geo overrides: two-letter ISO country code (geo.rs validation).
ALTER TABLE geo_overrides
    ADD CONSTRAINT chk_geo_overrides_country_code
        CHECK (char_length(country_code) = 2 AND country_code ~ '^[A-Za-z]{2}$');

-- Store products: ISO 4217 currency shape (store.rs valid_currency),
-- non-negative money and counters.
ALTER TABLE store_products
    ADD CONSTRAINT chk_store_products_currency
        CHECK (char_length(currency) = 3 AND currency ~ '^[A-Za-z]{3}$'),
    ADD CONSTRAINT chk_store_products_price
        CHECK (price >= 0),
    ADD CONSTRAINT chk_store_products_renewal_price
        CHECK (renewal_price IS NULL OR renewal_price >= 0),
    ADD CONSTRAINT chk_store_products_trial_days
        CHECK (trial_days >= 0),
    ADD CONSTRAINT chk_store_products_updates_years
        CHECK (updates_years IS NULL OR updates_years >= 0);

-- Pricing tiers: non-negative money and counters.
ALTER TABLE store_pricing_tiers
    ADD CONSTRAINT chk_store_pricing_tiers_price
        CHECK (price >= 0),
    ADD CONSTRAINT chk_store_pricing_tiers_renewal_price
        CHECK (renewal_price IS NULL OR renewal_price >= 0),
    ADD CONSTRAINT chk_store_pricing_tiers_trial_days
        CHECK (trial_days >= 0);

-- Coupons: percent discounts are (0, 100] (store.rs), fixed discounts
-- are non-negative amounts; counters never go negative.
ALTER TABLE store_coupons
    ADD CONSTRAINT chk_store_coupons_discount_value
        CHECK (discount_value >= 0
            AND (discount_type <> 'percent'
                OR (discount_value > 0 AND discount_value <= 100))),
    ADD CONSTRAINT chk_store_coupons_used_count
        CHECK (used_count >= 0),
    ADD CONSTRAINT chk_store_coupons_usage_limit
        CHECK (usage_limit IS NULL OR usage_limit >= 0);

-- Orders: ISO 4217 currency shape, non-negative money totals.
ALTER TABLE store_orders
    ADD CONSTRAINT chk_store_orders_currency
        CHECK (char_length(currency) = 3 AND currency ~ '^[A-Za-z]{3}$'),
    ADD CONSTRAINT chk_store_orders_subtotal
        CHECK (subtotal >= 0),
    ADD CONSTRAINT chk_store_orders_discount_total
        CHECK (discount_total >= 0),
    ADD CONSTRAINT chk_store_orders_total
        CHECK (total >= 0);

-- Order items: pricing model and billing period mirror the validated
-- values in store.rs; money amounts are non-negative.
ALTER TABLE store_order_items
    ADD CONSTRAINT chk_store_order_items_pricing_model
        CHECK (pricing_model IN ('one_time', 'subscription')),
    ADD CONSTRAINT chk_store_order_items_billing_period
        CHECK (billing_period IS NULL OR billing_period IN ('monthly', 'yearly')),
    ADD CONSTRAINT chk_store_order_items_unit_price
        CHECK (unit_price >= 0),
    ADD CONSTRAINT chk_store_order_items_line_total
        CHECK (line_total >= 0);

-- Entitlements: the downloads-used counter never goes negative.
ALTER TABLE store_entitlements
    ADD CONSTRAINT chk_store_entitlements_downloads_used
        CHECK (downloads_used >= 0);

-- Releases and files: counters and sizes never go negative.
ALTER TABLE software_releases
    ADD CONSTRAINT chk_software_releases_download_count
        CHECK (download_count >= 0);

ALTER TABLE release_files
    ADD CONSTRAINT chk_release_files_size_bytes
        CHECK (size_bytes >= 0);

-- Taxonomy: tree depth never goes negative.
ALTER TABLE software_taxonomy
    ADD CONSTRAINT chk_software_taxonomy_depth
        CHECK (depth >= 0);

-- ---------------------------------------------------------------------------
-- 4. Missing indexes: every foreign key and every filtered / searched
--    column gets one. Indexes that already exist are not repeated here.
-- ---------------------------------------------------------------------------
-- Foreign keys without an index.
CREATE INDEX IF NOT EXISTS idx_store_order_items_tier_id
    ON store_order_items (tier_id);
CREATE INDEX IF NOT EXISTS idx_store_order_items_software_id
    ON store_order_items (software_id);
CREATE INDEX IF NOT EXISTS idx_store_order_items_company_id
    ON store_order_items (company_id);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_product_id
    ON store_entitlements (product_id);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_tier_id
    ON store_entitlements (tier_id);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_software_id
    ON store_entitlements (software_id);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_release_id
    ON store_entitlements (release_id);
CREATE INDEX IF NOT EXISTS idx_store_downloads_order_id
    ON store_downloads (order_id);
CREATE INDEX IF NOT EXISTS idx_store_catalog_release_id
    ON store_catalog (release_id);
CREATE INDEX IF NOT EXISTS idx_store_issued_licenses_company_id
    ON store_issued_licenses (company_id);

-- Filtered / searched columns without an index.
CREATE INDEX IF NOT EXISTS idx_store_order_items_software_slug
    ON store_order_items (software_slug);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_license_key
    ON store_entitlements (license_key);
CREATE INDEX IF NOT EXISTS idx_store_downloads_software_slug
    ON store_downloads (software_slug);
CREATE INDEX IF NOT EXISTS idx_store_catalog_kind
    ON store_catalog (kind);
CREATE INDEX IF NOT EXISTS idx_software_releases_channel
    ON software_releases (channel);
CREATE INDEX IF NOT EXISTS idx_store_orders_provider
    ON store_orders (provider);
CREATE INDEX IF NOT EXISTS idx_store_coupons_expires_at
    ON store_coupons (expires_at);
CREATE INDEX IF NOT EXISTS idx_licenses_expires_at
    ON licenses (expires_at);
CREATE INDEX IF NOT EXISTS idx_store_entitlements_expires_at
    ON store_entitlements (expires_at);
CREATE INDEX IF NOT EXISTS idx_domains_ssl_expires_at
    ON domains (ssl_expires_at);

-- ---------------------------------------------------------------------------
-- 5. Missing unique constraints.
--    Safe on a pre-launch database: no production rows exist yet, and
--    both rules mirror uniqueness the application already assumes
--    (licenses.license_key is unique; one release per version + channel).
-- ---------------------------------------------------------------------------
CREATE UNIQUE INDEX IF NOT EXISTS uq_store_issued_licenses_license_key
    ON store_issued_licenses (license_key);

CREATE UNIQUE INDEX IF NOT EXISTS uq_software_releases_version_channel
    ON software_releases (version_id, channel);
