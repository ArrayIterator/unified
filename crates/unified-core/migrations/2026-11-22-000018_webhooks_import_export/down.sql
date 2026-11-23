-- Roll back 2026-11-22-000018_webhooks_import_export, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.

DROP TABLE IF EXISTS export_jobs;
DROP TABLE IF EXISTS import_jobs;
DROP TABLE IF EXISTS webhook_deliveries;
DROP TABLE IF EXISTS webhook_endpoints;
