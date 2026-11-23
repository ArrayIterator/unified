-- Down migration for the calendar module: drop exactly the
-- tables created by up.sql, in reverse dependency order
-- (there are no inter-table foreign keys, so order is cosmetic).

DROP TABLE IF EXISTS calendar_sources;
DROP TABLE IF EXISTS calendar_settings;
DROP TABLE IF EXISTS calendar_feeds;
