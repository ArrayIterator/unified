-- Documents metadata: human-facing metadata on the
-- `documents` row - title, alt_text, caption, description - plus
-- one `custom_metadata` JSONB column for caller-defined keys.
-- One table, not a separate metadata table: metadata is read with
-- the document row, never joined. The in-memory DashMap store is
-- authoritative at runtime; these columns are the durable
-- persistence targets (same convention as migration 000123).
--
-- `title` defaults to the uploaded file name at the application
-- layer (`insert_version` in `src/modules/documents/state/uploads.rs`);
-- the DB default is '' and existing rows are backfilled from `name`
-- below, so no row is left untitled. `alt_text` is the only
-- image-specific field; the rest apply to every file kind.
-- Custom keys match `[a-z0-9_]{1,64}` at the application layer
-- (`valid_metadata_key` in `src/modules/documents/validation.rs`);
-- the DB stores the merged object as-is. `notes` is the worked
-- custom-key example (a user-written description per document).

-- 1. `documents.title`: display title, backfilled from `name`.
ALTER TABLE IF EXISTS documents
    ADD COLUMN IF NOT EXISTS title TEXT NOT NULL DEFAULT '';
UPDATE documents SET title = name WHERE title = '';
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_title_length_check;
ALTER TABLE IF EXISTS documents
    ADD CONSTRAINT documents_title_length_check
    CHECK (char_length(title) >= 1 AND char_length(title) <= 512);

-- 2. Optional display text: alt text (images), caption, description.
ALTER TABLE IF EXISTS documents
    ADD COLUMN IF NOT EXISTS alt_text TEXT,
    ADD COLUMN IF NOT EXISTS caption TEXT,
    ADD COLUMN IF NOT EXISTS description TEXT;
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_alt_text_length_check;
ALTER TABLE IF EXISTS documents
    ADD CONSTRAINT documents_alt_text_length_check
    CHECK (alt_text IS NULL OR char_length(alt_text) <= 512);
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_caption_length_check;
ALTER TABLE IF EXISTS documents
    ADD CONSTRAINT documents_caption_length_check
    CHECK (caption IS NULL OR char_length(caption) <= 1024);
ALTER TABLE IF EXISTS documents
    DROP CONSTRAINT IF EXISTS documents_description_length_check;
ALTER TABLE IF EXISTS documents
    ADD CONSTRAINT documents_description_length_check
    CHECK (description IS NULL OR char_length(description) <= 4096);

-- 3. `documents.custom_metadata`: caller-defined keys, merged by
-- key at the application layer (a JSON null removes a key).
ALTER TABLE IF EXISTS documents
    ADD COLUMN IF NOT EXISTS custom_metadata JSONB NOT NULL DEFAULT '{}'::jsonb;

CREATE INDEX IF NOT EXISTS idx_documents_custom_metadata
    ON documents USING GIN (custom_metadata);
