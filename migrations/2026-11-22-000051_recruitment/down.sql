-- Roll back 2026-11-22-000051_recruitment, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS offers;
DROP TABLE IF EXISTS evaluations;
DROP TABLE IF EXISTS interviews;
DROP TABLE IF EXISTS applicant_stage_history;
DROP TABLE IF EXISTS applicants;
DROP TABLE IF EXISTS jobs;
