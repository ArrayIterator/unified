-- Roll back 2026-11-22-000086_manufacturing, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS mfg_stock_notices;
DROP TABLE IF EXISTS mfg_reservations;
DROP TABLE IF EXISTS mfg_production_runs;
DROP TABLE IF EXISTS mfg_units;
DROP TABLE IF EXISTS mfg_scrap_reasons;
DROP TABLE IF EXISTS mfg_work_order_requirements;
DROP TABLE IF EXISTS mfg_work_orders;
DROP TABLE IF EXISTS mfg_bom_routing_steps;
DROP TABLE IF EXISTS mfg_work_centers;
DROP TABLE IF EXISTS mfg_bom_version_lines;
DROP TABLE IF EXISTS mfg_bom_versions;
DROP TABLE IF EXISTS mfg_boms;
