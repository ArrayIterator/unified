-- Roll back 2026-11-22-000061_properties, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS property_block_history CASCADE;
DROP TABLE IF EXISTS property_blocks CASCADE;
DROP TABLE IF EXISTS property_plans CASCADE;
DROP TABLE IF EXISTS properties CASCADE;
