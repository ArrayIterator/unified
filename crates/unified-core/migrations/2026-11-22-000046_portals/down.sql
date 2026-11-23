-- Roll back 2026-11-22-000046_portals, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS portal_views;
DROP TABLE IF EXISTS portal_menus;
DROP TABLE IF EXISTS portals;
