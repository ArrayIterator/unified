-- Community / Forum tables for the Unified Pentagonal Project
-- (`src/modules/community.rs`): categories, topics, posts with
-- one-level nested replies and kept edit history, votes, flags,
-- moderation audit, members, and announcements.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/community.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `community_categories.visibility` is `public`,
--     `members_only`, or `staff_only`. `slug` is unique.
--   - `community_topics.pinned` / `locked` are moderator flags.
--   - `community_posts.parent_post_id` is NULL for a top-level
--     post and names a top-level post for a reply; replies are
--     exactly one level deep (enforced in the module, not by a
--     CHECK — SQL cannot express "the parent has no parent"
--     without a trigger). `hidden` posts are moderator- or
--     spam-hidden. `edited` marks a post whose body changed.
--   - `community_post_edits` keeps the previous body of every
--     edit (append-only history; the module never overwrites a
--     body without recording the previous one).
--   - `community_votes` has one row per (post, user); the unique
--     index enforces it. `value` is +1 (up) or -1 (down).
--   - `community_flags.status` is `open`, `dismissed`, or
--     `actioned`. The partial unique index enforces at most one
--     open flag per (post, reporter).
--   - `community_moderation_actions` is the audited moderator
--     log; `target_type` is `topic`, `post`, or `flag` and
--     `target_id` is polymorphic (no foreign key by design).
--   - `community_members.user_id` is a plain VARCHAR user id:
--     the module identifies callers by the `X-User-Id` header,
--     exactly like `store_orders.user_id` in migration 000002.
--     No foreign key to `users` is stored here. The directory
--     exposes display name, join date, and badge counts only.
--     `badge_count` is maintained by the
--     `gamification.badge.awarded` listener. `user_id` is unique.
--   - `community_announcements.published` is false until a
--     moderator publishes the announcement (which notifies every
--     member through `notification.requested`).
--   - Rate limits and the spam flag threshold live in the
--     module's in-memory `CommunitySettings` (configuration, not
--     rows); there is no settings table by design.

CREATE TABLE IF NOT EXISTS community_categories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    slug VARCHAR(128) NOT NULL UNIQUE,
    sort_order BIGINT NOT NULL DEFAULT 0,
    visibility VARCHAR(20) NOT NULL DEFAULT 'public' CHECK (visibility IN ('public', 'members_only', 'staff_only')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_categories_visibility_idx ON community_categories (visibility);
CREATE INDEX IF NOT EXISTS community_categories_sort_order_idx ON community_categories (sort_order);

CREATE TABLE IF NOT EXISTS community_topics (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category_id UUID NOT NULL REFERENCES community_categories (id) ON DELETE RESTRICT,
    title VARCHAR(200) NOT NULL,
    author_id VARCHAR(255) NOT NULL,
    tags TEXT[] NOT NULL DEFAULT '{}',
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    locked BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_topics_category_idx ON community_topics (category_id);
CREATE INDEX IF NOT EXISTS community_topics_author_idx ON community_topics (author_id);
CREATE INDEX IF NOT EXISTS community_topics_pinned_idx ON community_topics (pinned);
CREATE INDEX IF NOT EXISTS community_topics_created_at_idx ON community_topics (created_at);

CREATE TABLE IF NOT EXISTS community_posts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    topic_id UUID NOT NULL REFERENCES community_topics (id) ON DELETE CASCADE,
    parent_post_id UUID REFERENCES community_posts (id) ON DELETE CASCADE,
    author_id VARCHAR(255) NOT NULL,
    body TEXT NOT NULL,
    hidden BOOLEAN NOT NULL DEFAULT FALSE,
    edited BOOLEAN NOT NULL DEFAULT FALSE,
    edited_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_posts_topic_idx ON community_posts (topic_id);
CREATE INDEX IF NOT EXISTS community_posts_parent_idx ON community_posts (parent_post_id);
CREATE INDEX IF NOT EXISTS community_posts_author_idx ON community_posts (author_id);
CREATE INDEX IF NOT EXISTS community_posts_hidden_idx ON community_posts (hidden);
CREATE INDEX IF NOT EXISTS community_posts_created_at_idx ON community_posts (created_at);

CREATE TABLE IF NOT EXISTS community_post_edits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID NOT NULL REFERENCES community_posts (id) ON DELETE CASCADE,
    editor_id VARCHAR(255) NOT NULL,
    previous_body TEXT NOT NULL,
    edited_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_post_edits_post_idx ON community_post_edits (post_id);
CREATE INDEX IF NOT EXISTS community_post_edits_edited_at_idx ON community_post_edits (edited_at);

CREATE TABLE IF NOT EXISTS community_votes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID NOT NULL REFERENCES community_posts (id) ON DELETE CASCADE,
    user_id VARCHAR(255) NOT NULL,
    value SMALLINT NOT NULL CHECK (value IN (1, -1)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS community_votes_post_user_idx ON community_votes (post_id, user_id);
CREATE INDEX IF NOT EXISTS community_votes_post_idx ON community_votes (post_id);

CREATE TABLE IF NOT EXISTS community_flags (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID NOT NULL REFERENCES community_posts (id) ON DELETE CASCADE,
    reporter_id VARCHAR(255) NOT NULL,
    reason VARCHAR(1000) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'dismissed', 'actioned')),
    reviewed_by VARCHAR(255),
    reviewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_flags_post_idx ON community_flags (post_id);
CREATE INDEX IF NOT EXISTS community_flags_status_idx ON community_flags (status);
CREATE UNIQUE INDEX IF NOT EXISTS community_flags_open_post_reporter_idx ON community_flags (post_id, reporter_id) WHERE status = 'open';

CREATE TABLE IF NOT EXISTS community_moderation_actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_type VARCHAR(20) NOT NULL CHECK (target_type IN ('topic', 'post', 'flag')),
    target_id UUID NOT NULL,
    action VARCHAR(40) NOT NULL,
    moderator_id VARCHAR(255) NOT NULL,
    reason VARCHAR(1000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_moderation_actions_target_idx ON community_moderation_actions (target_type, target_id);
CREATE INDEX IF NOT EXISTS community_moderation_actions_moderator_idx ON community_moderation_actions (moderator_id);
CREATE INDEX IF NOT EXISTS community_moderation_actions_created_at_idx ON community_moderation_actions (created_at);

CREATE TABLE IF NOT EXISTS community_members (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR(255) NOT NULL UNIQUE,
    display_name VARCHAR(200) NOT NULL,
    badge_count BIGINT NOT NULL DEFAULT 0 CHECK (badge_count >= 0),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_members_joined_at_idx ON community_members (joined_at);

CREATE TABLE IF NOT EXISTS community_announcements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    body TEXT NOT NULL,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    published BOOLEAN NOT NULL DEFAULT FALSE,
    published_at TIMESTAMPTZ,
    author_id VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS community_announcements_published_idx ON community_announcements (published);
CREATE INDEX IF NOT EXISTS community_announcements_created_at_idx ON community_announcements (created_at);
