-- Roll back 2026-11-22-000044_affiliates, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS affiliate_commissions;
DROP TABLE IF EXISTS affiliate_conversions;
DROP TABLE IF EXISTS affiliate_clicks;
DROP TABLE IF EXISTS affiliate_links;
DROP TABLE IF EXISTS affiliate_commission_rules;
DROP TABLE IF EXISTS affiliate_payouts;
DROP TABLE IF EXISTS affiliates;
