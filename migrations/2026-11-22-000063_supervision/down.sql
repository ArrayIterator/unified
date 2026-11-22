-- Roll back 2026-11-22-000063_supervision, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core and the project tables
-- from 2026-11-22-000031_projects are untouched.

DROP TABLE IF EXISTS supervision_photos CASCADE;
DROP TABLE IF EXISTS supervision_daily_logs CASCADE;
DROP TABLE IF EXISTS supervision_approvals CASCADE;
DROP TABLE IF EXISTS supervision_defects CASCADE;
DROP TABLE IF EXISTS supervision_progress_reports CASCADE;
DROP TABLE IF EXISTS supervision_inspection_items CASCADE;
DROP TABLE IF EXISTS supervision_inspections CASCADE;
DROP TABLE IF EXISTS supervision_template_items CASCADE;
DROP TABLE IF EXISTS supervision_inspection_templates CASCADE;
DROP TABLE IF EXISTS supervision_assignments CASCADE;
