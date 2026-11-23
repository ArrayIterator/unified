-- Reverse of 000133: drop the tags + custom post type tables.
-- Entries and definitions are durable content, so dropping them
-- destroys data; this is only for rolling back an unreleased
-- migration, never for "deleting" a post type (that is a soft
-- disable: DELETE /api/v1/content/post-types/{id}).

DROP TABLE IF EXISTS content_custom_entries;
DROP TABLE IF EXISTS content_post_types;
DROP TABLE IF EXISTS content_post_tags;
DROP TABLE IF EXISTS content_tags;
