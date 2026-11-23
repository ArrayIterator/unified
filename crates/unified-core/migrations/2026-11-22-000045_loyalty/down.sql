-- Roll back 2026-11-22-000045_loyalty, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS loyalty_redemptions;
DROP TABLE IF EXISTS loyalty_ledger;
DROP TABLE IF EXISTS loyalty_tiers;
DROP TABLE IF EXISTS loyalty_earn_rules;
DROP TABLE IF EXISTS loyalty_programs;
