-- Roll back 2026-11-22-000050_assets, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS asset_stock_take_lines;
DROP TABLE IF EXISTS asset_stock_takes;
DROP TABLE IF EXISTS asset_stock_adjustments;
DROP TABLE IF EXISTS asset_stock;
DROP TABLE IF EXISTS asset_assignments;
DROP TABLE IF EXISTS assets;
