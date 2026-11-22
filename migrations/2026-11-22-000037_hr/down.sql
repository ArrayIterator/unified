-- Roll back 2026-11-22-000037_hr, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS hr_documents;
DROP TABLE IF EXISTS hr_checklist_items;
DROP TABLE IF EXISTS hr_checklists;
DROP TABLE IF EXISTS hr_reviews;
DROP TABLE IF EXISTS hr_review_cycles;
DROP TABLE IF EXISTS hr_payroll_items;
DROP TABLE IF EXISTS hr_payroll_runs;
DROP TABLE IF EXISTS hr_attendance_records;
DROP TABLE IF EXISTS hr_leave_requests;
DROP TABLE IF EXISTS hr_leave_balances;
DROP TABLE IF EXISTS hr_leave_types;
DROP TABLE IF EXISTS hr_contracts;
DROP TABLE IF EXISTS hr_positions;
DROP TABLE IF EXISTS hr_employees;
