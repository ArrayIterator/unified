-- Roll back 2026-11-22-000006_dashboards_reseller, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.

DROP TABLE IF EXISTS dashboard_settings;
DROP TABLE IF EXISTS view_as_sessions;
DROP TABLE IF EXISTS reseller_links;
