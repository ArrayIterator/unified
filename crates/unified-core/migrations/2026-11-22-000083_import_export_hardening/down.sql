-- Roll back 2026-11-22-000083_import_export_hardening, in reverse
-- dependency order. The tables from
-- 2026-11-22-000018_webhooks_import_export are left in place; only
-- the columns this migration added to them are dropped.

DROP INDEX IF EXISTS idx_export_jobs_company_id;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS rows_excluded_by_tenant_scope;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS has_more;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS row_offset;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS row_limit;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS redacted_fields;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS pii_redacted;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS company_id;
ALTER TABLE export_jobs DROP COLUMN IF EXISTS actor;

DROP INDEX IF EXISTS idx_import_jobs_company_id;
ALTER TABLE import_jobs DROP COLUMN IF EXISTS company_id;
ALTER TABLE import_jobs DROP COLUMN IF EXISTS actor;

DROP TABLE IF EXISTS import_sessions;
DROP TABLE IF EXISTS export_schedules;
DROP TABLE IF EXISTS mapping_templates;
