-- Roll back 2026-11-22-000019_maintenance_statuspage_announcements_auth,
-- in reverse dependency order. The core tables from
-- 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS google_drive_links;
DROP TABLE IF EXISTS google_accounts;
DROP TABLE IF EXISTS auth_recovery_codes;
DROP TABLE IF EXISTS announcement_read_receipts;
DROP TABLE IF EXISTS changelog_entries;
DROP TABLE IF EXISTS announcements;
DROP TABLE IF EXISTS status_incident_updates;
DROP TABLE IF EXISTS status_incidents;
DROP TABLE IF EXISTS status_daily_uptime;
DROP TABLE IF EXISTS status_components;
DROP TABLE IF EXISTS maintenance_history;
DROP TABLE IF EXISTS maintenance_windows;
