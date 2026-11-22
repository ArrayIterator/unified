-- Roll back 2026-11-22-000056_community, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS community_announcements;
DROP TABLE IF EXISTS community_members;
DROP TABLE IF EXISTS community_moderation_actions;
DROP TABLE IF EXISTS community_flags;
DROP TABLE IF EXISTS community_votes;
DROP TABLE IF EXISTS community_post_edits;
DROP TABLE IF EXISTS community_posts;
DROP TABLE IF EXISTS community_topics;
DROP TABLE IF EXISTS community_categories;
