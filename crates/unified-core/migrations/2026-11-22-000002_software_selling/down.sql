-- Roll back the software-selling tables, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS vcs_provider_settings;
DROP TABLE IF EXISTS vcs_sync_history;
DROP TABLE IF EXISTS vcs_version_drafts;
DROP TABLE IF EXISTS vcs_connections;
DROP TABLE IF EXISTS store_issued_licenses;
DROP TABLE IF EXISTS store_catalog;
DROP TABLE IF EXISTS store_downloads;
DROP TABLE IF EXISTS store_entitlements;
DROP TABLE IF EXISTS store_order_items;
DROP TABLE IF EXISTS store_orders;
DROP TABLE IF EXISTS store_coupons;
DROP TABLE IF EXISTS store_pricing_tiers;
DROP TABLE IF EXISTS store_products;
DROP TABLE IF EXISTS software_tag_assignments;
DROP TABLE IF EXISTS software_tags;
DROP TABLE IF EXISTS software_taxonomy;
DROP TABLE IF EXISTS release_files;
DROP TABLE IF EXISTS software_releases;
DROP TABLE IF EXISTS software_versions;
DROP TABLE IF EXISTS software;
