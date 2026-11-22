-- Roll back 2026-11-22-000022_cloudflare, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS cloudflare_purge_history;
DROP TABLE IF EXISTS cloudflare_settings;
