-- Down migration for 2026-11-22-000087_quality: drop the
-- Quality Management tables in reverse dependency order.
DROP TABLE IF EXISTS quality_capas;
DROP TABLE IF EXISTS quality_alerts;
DROP TABLE IF EXISTS quality_inspections;
DROP TABLE IF EXISTS quality_criteria;
DROP TABLE IF EXISTS quality_points;
