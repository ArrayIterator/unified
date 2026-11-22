-- Roll back 2026-11-22-000041_pricing_marketplace, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS marketplace_payouts;
DROP TABLE IF EXISTS marketplace_commissions;
DROP TABLE IF EXISTS marketplace_product_approvals;
DROP TABLE IF EXISTS marketplace_commission_rules;
DROP TABLE IF EXISTS marketplace_vendors;
DROP TABLE IF EXISTS pricing_tier_features;
DROP TABLE IF EXISTS pricing_tiers;
DROP TABLE IF EXISTS pricing_tables;
