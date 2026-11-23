-- Backup audit round: per-tenant scoping, recorded verification
-- and drill outcomes for backup jobs and runs (backup.rs).
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see the migration 000009 notes):
--   - `backup_jobs.company_id` / `backup_runs.company_id` mirror
--     the `company_id` fields added to `BackupJob` / `BackupRun`:
--     the owning company id (UUID string) from the `X-Company-Id`
--     header at creation. NULL means created by a platform admin
--     with no company. No foreign key: companies live in the
--     company module and a backup must stay readable after its
--     company record is removed.
--   - `backup_runs.verified`, `last_verified_at`,
--     `manifest_verified`, and `verification_error` record the
--     outcome of the last integrity verification (archive checksum
--     plus manifest cross-check). NULL means never verified.
--   - `backup_runs.last_drill_at` and `drill_verified` record the
--     outcome of the last restore drill. NULL means never drilled.
--   - `backup_runs.encryption` records the encryption mode in force
--     when the run was created (only `none` is implemented today;
--     see the backup module header).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

ALTER TABLE IF EXISTS backup_jobs
    ADD COLUMN IF NOT EXISTS company_id UUID;

CREATE INDEX IF NOT EXISTS idx_backup_jobs_company_id
    ON backup_jobs (company_id);

ALTER TABLE IF EXISTS backup_runs
    ADD COLUMN IF NOT EXISTS company_id UUID,
    ADD COLUMN IF NOT EXISTS verified BOOLEAN,
    ADD COLUMN IF NOT EXISTS last_verified_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS manifest_verified BOOLEAN,
    ADD COLUMN IF NOT EXISTS verification_error TEXT,
    ADD COLUMN IF NOT EXISTS last_drill_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS drill_verified BOOLEAN,
    ADD COLUMN IF NOT EXISTS encryption VARCHAR;

CREATE INDEX IF NOT EXISTS idx_backup_runs_company_id
    ON backup_runs (company_id);
CREATE INDEX IF NOT EXISTS idx_backup_runs_last_verified_at
    ON backup_runs (last_verified_at);
