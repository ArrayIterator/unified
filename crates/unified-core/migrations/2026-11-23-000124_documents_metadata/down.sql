-- Reverse of 000124: drop the metadata columns, their length
-- checks, and the custom_metadata GIN index.
-- The in-memory DashMap store is authoritative; dropping these
-- columns only removes the persistence targets.

DROP INDEX IF EXISTS idx_documents_custom_metadata;

ALTER TABLE IF EXISTS documents
    DROP COLUMN IF EXISTS custom_metadata;

ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_description_length_check;
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_caption_length_check;
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_alt_text_length_check;
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_title_length_check;

ALTER TABLE IF EXISTS documents
    DROP COLUMN IF EXISTS description;
ALTER TABLE IF EXISTS documents
    DROP COLUMN IF EXISTS caption;
ALTER TABLE IF EXISTS documents
    DROP COLUMN IF EXISTS alt_text;
ALTER TABLE IF EXISTS documents
    DROP COLUMN IF EXISTS title;
