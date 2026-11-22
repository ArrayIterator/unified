-- Roll back 2026-11-22-000016_email_newsletter, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.
--
-- Note: `mail_log` holds the foreign key to `email_templates`,
-- so the log goes before the templates.

DROP TABLE IF EXISTS newsletter_tracking_events;
DROP TABLE IF EXISTS newsletter_campaigns;
DROP TABLE IF EXISTS newsletter_subscribers;
DROP TABLE IF EXISTS newsletter_lists;
DROP TABLE IF EXISTS email_template_versions;
DROP TABLE IF EXISTS mail_log;
DROP TABLE IF EXISTS email_templates;
DROP TABLE IF EXISTS mail_smtp_settings;
DROP TABLE IF EXISTS email_send_log;
DROP TABLE IF EXISTS email_connections;
