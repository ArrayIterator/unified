-- Roll back 2026-11-22-000029_mcp, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS mcp_settings;
DROP TABLE IF EXISTS mcp_servers;
