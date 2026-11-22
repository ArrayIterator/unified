-- Roll back 2026-11-22-000003_schema_audit, in reverse order.
-- Restores the DOUBLE PRECISION money columns, drops the added
-- indexes, unique constraints, CHECK constraints, and timestamp columns.

-- 5. Unique constraints.
DROP INDEX IF EXISTS uq_software_releases_version_channel;
DROP INDEX IF EXISTS uq_store_issued_licenses_license_key;

-- 4. Indexes added by the audit migration.
DROP INDEX IF EXISTS idx_domains_ssl_expires_at;
DROP INDEX IF EXISTS idx_store_entitlements_expires_at;
DROP INDEX IF EXISTS idx_licenses_expires_at;
DROP INDEX IF EXISTS idx_store_coupons_expires_at;
DROP INDEX IF EXISTS idx_store_orders_provider;
DROP INDEX IF EXISTS idx_software_releases_channel;
DROP INDEX IF EXISTS idx_store_catalog_kind;
DROP INDEX IF EXISTS idx_store_downloads_software_slug;
DROP INDEX IF EXISTS idx_store_entitlements_license_key;
DROP INDEX IF EXISTS idx_store_order_items_software_slug;
DROP INDEX IF EXISTS idx_store_issued_licenses_company_id;
DROP INDEX IF EXISTS idx_store_catalog_release_id;
DROP INDEX IF EXISTS idx_store_downloads_order_id;
DROP INDEX IF EXISTS idx_store_entitlements_release_id;
DROP INDEX IF EXISTS idx_store_entitlements_software_id;
DROP INDEX IF EXISTS idx_store_entitlements_tier_id;
DROP INDEX IF EXISTS idx_store_entitlements_product_id;
DROP INDEX IF EXISTS idx_store_order_items_company_id;
DROP INDEX IF EXISTS idx_store_order_items_software_id;
DROP INDEX IF EXISTS idx_store_order_items_tier_id;

-- 3. CHECK constraints.
ALTER TABLE software_taxonomy
    DROP CONSTRAINT IF EXISTS chk_software_taxonomy_depth;
ALTER TABLE release_files
    DROP CONSTRAINT IF EXISTS chk_release_files_size_bytes;
ALTER TABLE software_releases
    DROP CONSTRAINT IF EXISTS chk_software_releases_download_count;
ALTER TABLE store_entitlements
    DROP CONSTRAINT IF EXISTS chk_store_entitlements_downloads_used;
ALTER TABLE store_order_items
    DROP CONSTRAINT IF EXISTS chk_store_order_items_line_total,
    DROP CONSTRAINT IF EXISTS chk_store_order_items_unit_price,
    DROP CONSTRAINT IF EXISTS chk_store_order_items_billing_period,
    DROP CONSTRAINT IF EXISTS chk_store_order_items_pricing_model;
ALTER TABLE store_orders
    DROP CONSTRAINT IF EXISTS chk_store_orders_total,
    DROP CONSTRAINT IF EXISTS chk_store_orders_discount_total,
    DROP CONSTRAINT IF EXISTS chk_store_orders_subtotal,
    DROP CONSTRAINT IF EXISTS chk_store_orders_currency;
ALTER TABLE store_coupons
    DROP CONSTRAINT IF EXISTS chk_store_coupons_usage_limit,
    DROP CONSTRAINT IF EXISTS chk_store_coupons_used_count,
    DROP CONSTRAINT IF EXISTS chk_store_coupons_discount_value;
ALTER TABLE store_pricing_tiers
    DROP CONSTRAINT IF EXISTS chk_store_pricing_tiers_trial_days,
    DROP CONSTRAINT IF EXISTS chk_store_pricing_tiers_renewal_price,
    DROP CONSTRAINT IF EXISTS chk_store_pricing_tiers_price;
ALTER TABLE store_products
    DROP CONSTRAINT IF EXISTS chk_store_products_updates_years,
    DROP CONSTRAINT IF EXISTS chk_store_products_trial_days,
    DROP CONSTRAINT IF EXISTS chk_store_products_renewal_price,
    DROP CONSTRAINT IF EXISTS chk_store_products_price,
    DROP CONSTRAINT IF EXISTS chk_store_products_currency;
ALTER TABLE geo_overrides
    DROP CONSTRAINT IF EXISTS chk_geo_overrides_country_code;
ALTER TABLE domains
    DROP CONSTRAINT IF EXISTS chk_domains_ssl_status;
ALTER TABLE licenses
    DROP CONSTRAINT IF EXISTS chk_licenses_max_activations,
    DROP CONSTRAINT IF EXISTS chk_licenses_status;

-- 2. Money: NUMERIC(19,4) -> DOUBLE PRECISION.
--    Values stored with 4 fractional digits convert back exactly in
--    practice; the cast is lossy only for values the NUMERIC column
--    could hold with more precision than DOUBLE PRECISION can keep.
ALTER TABLE store_order_items
    ALTER COLUMN unit_price TYPE DOUBLE PRECISION USING unit_price::double precision,
    ALTER COLUMN line_total TYPE DOUBLE PRECISION USING line_total::double precision;
ALTER TABLE store_orders
    ALTER COLUMN subtotal TYPE DOUBLE PRECISION USING subtotal::double precision,
    ALTER COLUMN discount_total TYPE DOUBLE PRECISION USING discount_total::double precision,
    ALTER COLUMN total TYPE DOUBLE PRECISION USING total::double precision;
ALTER TABLE store_coupons
    ALTER COLUMN discount_value TYPE DOUBLE PRECISION USING discount_value::double precision;
ALTER TABLE store_pricing_tiers
    ALTER COLUMN price TYPE DOUBLE PRECISION USING price::double precision,
    ALTER COLUMN renewal_price TYPE DOUBLE PRECISION USING renewal_price::double precision;
ALTER TABLE store_products
    ALTER COLUMN price TYPE DOUBLE PRECISION USING price::double precision,
    ALTER COLUMN renewal_price TYPE DOUBLE PRECISION USING renewal_price::double precision;

-- 1. Timestamp columns added by the audit migration.
ALTER TABLE license_activations
    DROP COLUMN IF EXISTS updated_at;
ALTER TABLE software_tag_assignments
    DROP COLUMN IF EXISTS updated_at;
ALTER TABLE user_roles
    DROP COLUMN IF EXISTS updated_at;
ALTER TABLE vcs_sync_history
    DROP COLUMN IF EXISTS updated_at,
    DROP COLUMN IF EXISTS created_at;
ALTER TABLE store_issued_licenses
    DROP COLUMN IF EXISTS updated_at;
ALTER TABLE store_downloads
    DROP COLUMN IF EXISTS updated_at,
    DROP COLUMN IF EXISTS created_at;
ALTER TABLE store_order_items
    DROP COLUMN IF EXISTS updated_at,
    DROP COLUMN IF EXISTS created_at;
ALTER TABLE software_releases
    DROP COLUMN IF EXISTS updated_at;
