-- Roll back 2026-11-22-000065_documents, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS document_quotas CASCADE;
DROP TABLE IF EXISTS document_downloads CASCADE;
DROP TABLE IF EXISTS document_allowlist_rules CASCADE;
DROP TABLE IF EXISTS document_upload_parts CASCADE;
DROP TABLE IF EXISTS document_upload_sessions CASCADE;
DROP TABLE IF EXISTS document_versions CASCADE;
DROP TABLE IF EXISTS documents CASCADE;
