-- Reverse of 000123: drop `finalize_jobs`, restore NOT NULL
-- checksums and the original session status set, drop the
-- `visibility` column and its indexes.
-- The in-memory DashMap store is authoritative; dropping these
-- tables/columns only removes the persistence targets.
-- NOTE: the `SET NOT NULL` steps fail if NULL checksums exist
-- (lazy-finalize rows); purge or verify those documents first.

DROP TABLE IF EXISTS finalize_jobs;

ALTER TABLE IF EXISTS document_upload_sessions
    DROP CONSTRAINT IF EXISTS document_upload_sessions_status_check;
ALTER TABLE IF EXISTS document_upload_sessions
    ADD CONSTRAINT document_upload_sessions_status_check
    CHECK (status IN ('active', 'completed', 'aborted', 'expired'));

ALTER TABLE IF EXISTS document_versions
    DROP CONSTRAINT IF EXISTS document_versions_checksum_check;
ALTER TABLE IF EXISTS document_versions
    ALTER COLUMN checksum SET NOT NULL;
ALTER TABLE IF EXISTS document_versions
    ADD CONSTRAINT document_versions_checksum_check
    CHECK (checksum ~ '^[0-9a-f]{64}$');

ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_checksum_check;
ALTER TABLE IF EXISTS documents
    ALTER COLUMN checksum SET NOT NULL;
ALTER TABLE IF EXISTS documents
    ADD CONSTRAINT documents_checksum_check
    CHECK (checksum ~ '^[0-9a-f]{64}$');

DROP INDEX IF EXISTS idx_documents_storage_key;
DROP INDEX IF EXISTS idx_documents_visibility;
ALTER TABLE IF EXISTS documents
    DROP COLUMN IF EXISTS visibility;
