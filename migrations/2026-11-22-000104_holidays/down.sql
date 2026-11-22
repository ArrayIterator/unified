-- Down migration for 2026-11-22-000104_holidays: drop the
-- Holidays tables in reverse dependency order.
DROP TABLE IF EXISTS holiday_overrides;
DROP TABLE IF EXISTS holidays;
DROP TABLE IF EXISTS holiday_sync_runs;
DROP TABLE IF EXISTS holiday_sync_configs;
DROP TABLE IF EXISTS holiday_countries;
DROP TABLE IF EXISTS holiday_providers;
