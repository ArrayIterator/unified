-- Roll back 2026-11-22-000013_panels, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS panel_synced_records;
DROP TABLE IF EXISTS panel_connections;
