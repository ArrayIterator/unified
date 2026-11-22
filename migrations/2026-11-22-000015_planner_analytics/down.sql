-- Roll back 2026-11-22-000015_planner_analytics, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.

DROP TABLE IF EXISTS analytics_scheduled_reports;
DROP TABLE IF EXISTS analytics_report_runs;
DROP TABLE IF EXISTS analytics_reports;
DROP TABLE IF EXISTS analytics_health_samples;
DROP TABLE IF EXISTS analytics_metric_samples;
DROP TABLE IF EXISTS analytics_license_snapshots;
DROP TABLE IF EXISTS analytics_sales;
DROP TABLE IF EXISTS allocation_plans;
DROP TABLE IF EXISTS pool_usage_samples;
DROP TABLE IF EXISTS capacity_pools;
