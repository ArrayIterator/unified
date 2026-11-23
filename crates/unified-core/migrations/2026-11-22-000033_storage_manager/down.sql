-- Roll back 2026-11-22-000033_storage_manager, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.
-- WARNING: this drops the Storage Manager tables and all rows in them.
-- Bucket credentials are stored encrypted in storage_buckets; dropping
-- the table destroys them, and the stored objects themselves (in S3,
-- Drive, SFTP, or on local disk) are NOT deleted by this rollback.

DROP TABLE IF EXISTS storage_health_checks;
DROP TABLE IF EXISTS storage_signed_url_revocations;
DROP TABLE IF EXISTS storage_lifecycle_rules;
DROP TABLE IF EXISTS storage_usage_daily;
DROP TABLE IF EXISTS storage_quotas;
DROP TABLE IF EXISTS storage_bindings;
DROP TABLE IF EXISTS storage_buckets;
