-- Roll back 2026-11-22-000007_hosting, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS hosting_order_claims;
DROP TABLE IF EXISTS hosting_accounts;
DROP TABLE IF EXISTS hosting_plans;
