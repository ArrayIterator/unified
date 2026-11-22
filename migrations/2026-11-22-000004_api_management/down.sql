-- Roll back 2026-11-22-000004_api_management, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.

DROP TABLE IF EXISTS api_access_audit;
DROP TABLE IF EXISTS api_access_rules;
DROP TABLE IF EXISTS api_keys;
DROP TABLE IF EXISTS api_prefix_settings;
DROP TABLE IF EXISTS api_endpoints;
DROP TABLE IF EXISTS api_versions;
