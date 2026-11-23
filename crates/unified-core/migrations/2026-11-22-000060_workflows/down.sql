-- Roll back 2026-11-22-000060_workflows, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS workflow_run_steps;
DROP TABLE IF EXISTS workflow_runs;
DROP TABLE IF EXISTS workflows;
