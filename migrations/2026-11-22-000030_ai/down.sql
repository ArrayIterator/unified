-- Roll back 2026-11-22-000030_ai, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS ai_drafts;
DROP TABLE IF EXISTS ai_usage_records;
DROP TABLE IF EXISTS ai_provider_connections;
