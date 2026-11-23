-- Rollback for 2026-11-22-000082_cluster_sync_reliability.
-- Drops only the tables this migration created. The base cluster
-- tables from 000008_clusters are left untouched.

DROP TABLE IF EXISTS cluster_sync_domains;
DROP TABLE IF EXISTS cluster_sync_settings;
DROP TABLE IF EXISTS cluster_sync_idempotency_keys;
DROP TABLE IF EXISTS cluster_sync_retries;
DROP TABLE IF EXISTS cluster_sync_conflicts;
