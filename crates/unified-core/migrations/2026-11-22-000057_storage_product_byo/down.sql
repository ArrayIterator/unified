-- Roll back 2026-11-22-000057_storage_product_byo, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core and the Storage Manager
-- tables from 2026-11-22-000033_storage_manager are untouched.
-- WARNING: this drops the storage-product and BYO tables and all rows
-- in them. BYO credentials are stored encrypted in
-- storage_byo_connections; dropping the table destroys them, and the
-- stored objects themselves (in S3, Drive, SFTP, or on local disk)
-- are NOT deleted by this rollback.

DROP TABLE IF EXISTS storage_byo_health_checks;
DROP TABLE IF EXISTS storage_byo_objects;
DROP TABLE IF EXISTS storage_byo_terms_acceptances;
DROP TABLE IF EXISTS storage_byo_terms;
DROP TABLE IF EXISTS storage_byo_connections;
DROP TABLE IF EXISTS storage_usage_metering;
DROP TABLE IF EXISTS storage_plan_assignments;
DROP TABLE IF EXISTS storage_plans;
