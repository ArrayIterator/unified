-- Roll back migration 000132: drop the documents
-- folder link and the folder table. Documents themselves are
-- untouched; only their folder assignment is lost.
ALTER TABLE IF EXISTS documents
    DROP COLUMN IF EXISTS folder_id;

DROP TABLE IF EXISTS document_folders;
