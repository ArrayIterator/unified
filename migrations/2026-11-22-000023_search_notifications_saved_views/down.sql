-- Roll back 2026-11-22-000023_search_notifications_saved_views,
-- in reverse dependency order. The core tables from
-- 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS timeline_entries;
DROP TABLE IF EXISTS saved_views;
DROP TABLE IF EXISTS notification_templates;
DROP TABLE IF EXISTS notification_preferences;
DROP TABLE IF EXISTS notifications;
DROP TABLE IF EXISTS search_documents;
