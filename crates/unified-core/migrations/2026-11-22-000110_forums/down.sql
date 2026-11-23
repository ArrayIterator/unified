-- Down migration for 2026-11-22-000110_forums: drop all forums tables.
DROP TABLE IF EXISTS forum_settings;
DROP TABLE IF EXISTS forum_reputation;
DROP TABLE IF EXISTS forum_poll_votes;
DROP TABLE IF EXISTS forum_poll_options;
DROP TABLE IF EXISTS forum_polls;
DROP TABLE IF EXISTS forum_subscriptions;
DROP TABLE IF EXISTS forum_moderation_actions;
DROP TABLE IF EXISTS forum_reports;
DROP TABLE IF EXISTS forum_votes;
DROP TABLE IF EXISTS forum_post_edits;
DROP TABLE IF EXISTS forum_posts;
DROP TABLE IF EXISTS forum_threads;
DROP TABLE IF EXISTS forum_categories;
DROP TABLE IF EXISTS forums;
