-- Roll back 2026-11-22-000047_forms, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS form_submission_values;
DROP TABLE IF EXISTS form_submissions;
DROP TABLE IF EXISTS form_fields;
DROP TABLE IF EXISTS forms;
