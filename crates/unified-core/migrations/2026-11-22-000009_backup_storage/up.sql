-- Backup and storage-target tables for the Unified Pentagonal
-- Project (backup.rs, storage.rs): backup jobs and their runs, and
-- the named storage targets backups and assets are written to.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `backup_jobs.kind` is `database`, `files`, `config`, or
--     `full`; `status` is `pending`, `running`, `completed`, or
--     `failed`. The denormalized last-run summary columns
--     (`status`, `checksum`, `size_bytes`, `storage_key`,
--     `last_run_id`, `started_at`, `finished_at`, `error`) mirror
--     the `BackupJob` struct exactly.
--   - `backup_jobs.scheduling` is the JSON form of the last
--     `scheduler.enqueue` outcome (`{enqueued, message}`).
--   - `backup_runs` has no `updated_at` in the struct (a run is
--     append-only once finished); the column is added here per the
--     project convention that every table carries both timestamps,
--     and normally equals `created_at`.
--   - `storage_targets` is keyed by `name`: the target name is the
--     identity in code. `config` stores the masked configuration
--     (secrets are write-only and never returned); the stored JSON
--     must already be masked, exactly as `StorageTargetConfig::
--     masked_config` produces it. `kind` is `local`, `s3`, `drive`,
--     or `r2` (see `StorageKind` in `crate::core::storage`).
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- A backup job: what to back up, where to send it, when to run it,
-- and how much history to keep.
CREATE TABLE IF NOT EXISTS backup_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('database', 'files', 'config', 'full')),
    database_name VARCHAR NOT NULL DEFAULT '',
    paths JSONB NOT NULL DEFAULT '[]'::jsonb,
    config_keys JSONB NOT NULL DEFAULT '[]'::jsonb,
    target VARCHAR NOT NULL,
    schedule VARCHAR NOT NULL DEFAULT '',
    retention_keep_last INTEGER NOT NULL DEFAULT 7 CHECK (retention_keep_last >= 0),
    retention_keep_days INTEGER NOT NULL DEFAULT 30 CHECK (retention_keep_days >= 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    scheduling JSONB NOT NULL DEFAULT '{"enqueued": false, "message": ""}'::jsonb,
    status VARCHAR NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    checksum VARCHAR,
    size_bytes BIGINT CHECK (size_bytes >= 0),
    storage_key VARCHAR,
    -- References backup_runs; the foreign key is added by ALTER
    -- TABLE below, after both tables exist (the two tables
    -- reference each other: runs point at their job, jobs point at
    -- their latest run).
    last_run_id UUID,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_backup_jobs_target
    ON backup_jobs (target);
CREATE INDEX IF NOT EXISTS idx_backup_jobs_status
    ON backup_jobs (status);
CREATE INDEX IF NOT EXISTS idx_backup_jobs_enabled
    ON backup_jobs (enabled);

-- One execution of a backup job. `job_name`, `kind`, and `target`
-- are denormalized from the job so a run stays readable after its
-- job is deleted (the foreign key then becomes NULL).
CREATE TABLE IF NOT EXISTS backup_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID REFERENCES backup_jobs (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    job_name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('database', 'files', 'config', 'full')),
    target VARCHAR NOT NULL,
    storage_key VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    checksum VARCHAR,
    size_bytes BIGINT CHECK (size_bytes >= 0),
    file_count BIGINT CHECK (file_count >= 0),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_backup_runs_job_id
    ON backup_runs (job_id);
CREATE INDEX IF NOT EXISTS idx_backup_runs_status
    ON backup_runs (status);
CREATE INDEX IF NOT EXISTS idx_backup_runs_started_at
    ON backup_runs (started_at);

-- A named storage target: local directory, S3-compatible bucket,
-- Google Drive folder, or Cloudflare R2 bucket.
CREATE TABLE IF NOT EXISTS storage_targets (
    name VARCHAR PRIMARY KEY,
    kind VARCHAR NOT NULL CHECK (kind IN ('local', 's3', 'drive', 'r2')),
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_storage_targets_kind
    ON storage_targets (kind);

-- Circular reference between the two tables: a run points at its
-- job, a job points at its latest run. Added here, once both
-- tables exist.
ALTER TABLE backup_jobs
    ADD CONSTRAINT fk_backup_jobs_last_run_id
    FOREIGN KEY (last_run_id) REFERENCES backup_runs (id) ON DELETE SET NULL ON UPDATE NO ACTION;
