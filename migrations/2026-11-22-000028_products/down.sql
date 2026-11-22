-- Roll back 2026-11-22-000028_products, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS product_tag_assignments;
DROP TABLE IF EXISTS product_tags;
DROP TABLE IF EXISTS product_versions;
DROP TABLE IF EXISTS product_files;
DROP TABLE IF EXISTS product_inventory;
DROP TABLE IF EXISTS product_tax_rules;
DROP TABLE IF EXISTS product_usage_prices;
DROP TABLE IF EXISTS product_regional_prices;
DROP TABLE IF EXISTS product_price_tiers;
DROP TABLE IF EXISTS product_bundle_items;
DROP TABLE IF EXISTS product_variants;
DROP TABLE IF EXISTS product_options;
DROP TABLE IF EXISTS products;
DROP TABLE IF EXISTS product_taxonomy;
