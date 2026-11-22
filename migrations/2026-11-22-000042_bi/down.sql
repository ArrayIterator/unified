-- Down migration for 2026-11-22-000042_bi: drop the Business
-- Intelligence tables in reverse dependency order (schedules
-- reference reports, so schedules go first).

DROP TABLE IF EXISTS bi_report_schedules;
DROP TABLE IF EXISTS bi_kpi_dashboards;
DROP TABLE IF EXISTS bi_reports;
