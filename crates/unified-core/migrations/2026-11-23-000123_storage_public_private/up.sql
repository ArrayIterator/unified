-- Storage public/private (`src/modules/documents/`): visibility enum
-- column, nullable checksums (lazy finalize), session status values
-- for the lazy-finalize flow, and the finalize_jobs table.
-- The in-memory DashMap store is authoritative at runtime; these
-- tables are the durable persistence targets (see C11).

-- 1. `documents.visibility`: enum public/private, default private.
-- The user calls this "is_public"; it is an enum column, NOT a
-- boolean, so a future `unlisted` value needs no schema change.
ALTER TABLE IF EXISTS documents
    ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'private'
    CHECK (visibility IN ('public', 'private'));

CREATE INDEX IF NOT EXISTS idx_documents_visibility
    ON documents (visibility);
CREATE INDEX IF NOT EXISTS idx_documents_storage_key
    ON documents (storage_key);

-- 2. Nullable checksums (lazy finalize: NULL = not hashed yet).
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_checksum_check;
ALTER TABLE IF EXISTS documents
    ALTER COLUMN checksum DROP NOT NULL;
ALTER TABLE IF EXISTS documents
    ADD CONSTRAINT documents_checksum_check
    CHECK (checksum IS NULL OR checksum ~ '^[0-9a-f]{64}$');

ALTER TABLE IF EXISTS document_versions
    DROP CONSTRAINT IF EXISTS document_versions_checksum_check;
ALTER TABLE IF EXISTS document_versions
    ALTER COLUMN checksum DROP NOT NULL;
ALTER TABLE IF EXISTS document_versions
    ADD CONSTRAINT document_versions_checksum_check
    CHECK (checksum IS NULL OR checksum ~ '^[0-9a-f]{64}$');

-- 3. Session statuses for the lazy-finalize flow: `completing`
-- (already used at runtime but missing from the old CHECK),
-- `received` (all parts received, finalize job pending) and
-- `failed` (finalize mismatch).
ALTER TABLE IF EXISTS document_upload_sessions
    DROP CONSTRAINT IF EXISTS document_upload_sessions_status_check;
ALTER TABLE IF EXISTS document_upload_sessions
    ADD CONSTRAINT document_upload_sessions_status_check
    CHECK (status IN ('active', 'completing', 'received', 'completed', 'failed', 'aborted', 'expired'));

-- 4. `finalize_jobs`: the durable record of pending finalize jobs.
-- One row per lazy-finalize job, created when a chunked session
-- completes (all parts received). The worker claims a row
-- atomically (`pending` -> `finalizing`, one winner), hashes the
-- stored object, then marks it `verified` (checksum stored on the
-- document) or `failed` (mismatch: object quarantined/deleted, user
-- notified). A stale claim (`finalizing` with an old `claimed_at`)
-- is reaped back to `pending`. The startup re-scan re-enqueues
-- sessions that have no `pending`/`finalizing` row.
CREATE TABLE IF NOT EXISTS finalize_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES document_upload_sessions (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    document_id UUID REFERENCES documents (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'finalizing', 'verified', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    claimed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_finalize_jobs_session_id
    ON finalize_jobs (session_id);
CREATE INDEX IF NOT EXISTS idx_finalize_jobs_status
    ON finalize_jobs (status);
CREATE INDEX IF NOT EXISTS idx_finalize_jobs_document_id
    ON finalize_jobs (document_id);
