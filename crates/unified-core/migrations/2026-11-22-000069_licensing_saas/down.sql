-- Roll back 2026-11-22-000069_licensing_saas, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core and the licenses /
-- license_activations tables are untouched.

DROP TABLE IF EXISTS license_activation_history CASCADE;
DROP TABLE IF EXISTS license_offline_tokens CASCADE;
DROP TABLE IF EXISTS license_events CASCADE;
DROP TABLE IF EXISTS license_releases CASCADE;
DROP TABLE IF EXISTS license_distributions CASCADE;
DROP TABLE IF EXISTS license_products CASCADE;
