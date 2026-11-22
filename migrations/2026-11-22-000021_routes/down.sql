-- Roll back 2026-11-22-000021_routes, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS route_redirects;
DROP TABLE IF EXISTS named_routes;
