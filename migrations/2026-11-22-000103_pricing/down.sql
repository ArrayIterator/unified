-- Down migration for 2026-11-22-000103_pricing: drop the
-- Smart Pricing tables in reverse dependency order.
-- (16 CREATE TABLE statements in up.sql, 16 DROP TABLE statements here.)

DROP TABLE IF EXISTS pricing_reconciliation_reports;
DROP TABLE IF EXISTS pricing_credit_notes;
DROP TABLE IF EXISTS pricing_proration_records;
DROP TABLE IF EXISTS pricing_price_approvals;
DROP TABLE IF EXISTS pricing_price_versions;
DROP TABLE IF EXISTS pricing_reference_prices;
DROP TABLE IF EXISTS pricing_bundle_components;
DROP TABLE IF EXISTS pricing_bundles;
DROP TABLE IF EXISTS pricing_tiers;
DROP TABLE IF EXISTS pricing_cost_lines;
DROP TABLE IF EXISTS pricing_recommendations;
DROP TABLE IF EXISTS pricing_coupon_redemptions;
DROP TABLE IF EXISTS pricing_coupons;
DROP TABLE IF EXISTS pricing_rules;
DROP TABLE IF EXISTS pricing_currencies;
DROP TABLE IF EXISTS pricing_settings;
