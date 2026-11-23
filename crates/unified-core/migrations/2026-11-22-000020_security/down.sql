-- Roll back 2026-11-22-000020_security, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS security_failed_auth_clusters;
DROP TABLE IF EXISTS security_reputation;
DROP TABLE IF EXISTS security_ip_lists;
