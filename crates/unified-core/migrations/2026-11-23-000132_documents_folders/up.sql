-- Documents folders: hierarchical folders for the
-- documents media library, plus `documents.folder_id` linking each
-- document to its folder ('' = library root).
--
-- The in-memory DashMap store is authoritative at runtime (same
-- convention as migrations 000123/000124); these tables are the
-- durable persistence targets. Folders are tenant-scoped through
-- `company_id` (empty = unscoped, same convention as the
-- `documents` row); the application layer enforces company scope
-- on every folder read and mutation, and a folder id is never
-- trusted from a client without an existence + scope check.
--
-- No merge with the files module store: this is the documents
-- module's own folder tree (`document_folders`), separate from
-- the files module's records. The files-vs-documents merge is an
-- open decision and is deliberately out of scope here.

-- 1. `document_folders`: one row per folder. `parent_id` is ''
-- for a top-level folder; nesting is by id reference, not by
-- path, so a rename never re-keys children. The application layer
-- rejects cycles (a folder may not move into itself or one of its
-- descendants) and refuses to delete a folder that still holds
-- documents or subfolders (409).
CREATE TABLE IF NOT EXISTS document_folders (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (char_length(name) >= 1 AND char_length(name) <= 128),
    parent_id TEXT NOT NULL DEFAULT '',
    company_id TEXT NOT NULL DEFAULT '',
    owner_id TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_document_folders_parent
    ON document_folders (parent_id);
CREATE INDEX IF NOT EXISTS idx_document_folders_company
    ON document_folders (company_id);

-- 2. `documents.folder_id`: the folder a document lives in
-- ('' = library root). Existing rows stay at the root.
ALTER TABLE IF EXISTS documents
    ADD COLUMN IF NOT EXISTS folder_id TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_documents_folder
    ON documents (folder_id);
