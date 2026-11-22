-- Roll back 2026-11-22-000009_backup_storage, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.
--
-- Note: `backup_jobs.last_run_id` references `backup_runs`, while
-- `backup_runs.job_id` references `backup_jobs`. `backup_jobs`
-- must go first: dropping `backup_runs` first would fail because
-- `backup_jobs` still holds a foreign key to it.

DROP TABLE IF EXISTS backup_jobs;
DROP TABLE IF EXISTS backup_runs;
DROP TABLE IF EXISTS storage_targets;
