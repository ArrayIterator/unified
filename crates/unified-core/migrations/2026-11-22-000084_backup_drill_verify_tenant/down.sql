-- Roll back 2026-11-22-000084_backup_drill_verify_tenant, in
-- reverse dependency order. The backup tables from
-- 2026-11-22-000009_backup_storage are untouched.

DROP INDEX IF EXISTS idx_backup_runs_last_verified_at;
DROP INDEX IF EXISTS idx_backup_runs_company_id;
DROP INDEX IF EXISTS idx_backup_jobs_company_id;

ALTER TABLE IF EXISTS backup_runs
    DROP COLUMN IF EXISTS encryption,
    DROP COLUMN IF EXISTS drill_verified,
    DROP COLUMN IF EXISTS last_drill_at,
    DROP COLUMN IF EXISTS verification_error,
    DROP COLUMN IF EXISTS manifest_verified,
    DROP COLUMN IF EXISTS last_verified_at,
    DROP COLUMN IF EXISTS verified,
    DROP COLUMN IF EXISTS company_id;

ALTER TABLE IF EXISTS backup_jobs
    DROP COLUMN IF EXISTS company_id;
