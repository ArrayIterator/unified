-- Roll back 2026-11-22-000010_providers, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS provider_servers;
DROP TABLE IF EXISTS provider_connections;
