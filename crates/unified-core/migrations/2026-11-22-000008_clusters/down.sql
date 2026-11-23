-- Roll back 2026-11-22-000008_clusters, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS cluster_health_checks;
DROP TABLE IF EXISTS cluster_sync_logs;
DROP TABLE IF EXISTS cluster_sync_items;
DROP TABLE IF EXISTS cluster_sync_plans;
DROP TABLE IF EXISTS cluster_nodes;
DROP TABLE IF EXISTS clusters;
