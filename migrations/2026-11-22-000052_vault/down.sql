-- Roll back migration 2026-11-22-000052_vault. Drops the tables in
-- reverse dependency order. `vault_documents` and
-- `vault_document_versions` reference each other (a document points
-- at its current version; a version points at its document), so
-- both are dropped CASCADE. The access log is append-only in the
-- module, but the rollback still drops it: a rollback is an
-- explicit operator action, not a module delete.
DROP TABLE IF EXISTS vault_access_requests;
DROP TABLE IF EXISTS vault_share_links;
DROP TABLE IF EXISTS vault_access_log;
DROP TABLE IF EXISTS vault_documents CASCADE;
DROP TABLE IF EXISTS vault_document_versions CASCADE;
DROP TABLE IF EXISTS vault_folders;
