-- Forums module (`src/modules/forums/`): the public-facing discussion-forum
-- product. PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000056_community and
-- 000112_payment_idempotency_claims: the in-memory DashMap store is
-- still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz. `user_id`
--     / `author_id` / `reporter_id` / `moderator_id` are plain TEXT
--     with NO foreign key on purpose, matching the community
--     convention: users live in the users module and this module
--     must not assume its table or migration order.
--   - `tenant_id` is plain TEXT on every row: forums data is
--     tenant business data and every query filters by it. A row
--     from another tenant reads as 404, never 403.
--   - Money never appears here: forums are free to read and post;
--     paid access is sold through the store/licensing modules.
--   - Unique indexes enforce the module invariants in the database
--     too: one slug per (tenant, forum) and one slug per
--     (forum, category); one vote per (post, user); one
--     subscription per (tenant, user, target); at most one poll
--     per thread; one poll vote per (poll, user); one reputation
--     row per (tenant, user).
--   - `forum_posts.depth` is 0 (opening post), 1 (reply), or 2
--     (nested reply); deeper nesting is rejected by the module.
--     `attachment_ids` references documents/storage objects by
--     id only (metadata); this module never stores file bytes.
--   - `forum_reports.status` is `open`, `dismissed`, or
--     `actioned`; `forum_threads.solved` is set together with
--     `best_answer_post_id` (at most one best answer per thread).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Forums: public / members-only / staff-only discussion spaces.
CREATE TABLE IF NOT EXISTS forums (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 128),
    icon TEXT,
    visibility TEXT NOT NULL DEFAULT 'public' CHECK (visibility IN ('public', 'members_only', 'staff_only')),
    sort_order BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_forums_tenant_slug ON forums (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_forums_tenant_visibility ON forums (tenant_id, visibility, sort_order);

-- Categories inside a forum.
CREATE TABLE IF NOT EXISTS forum_categories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    forum_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 128),
    sort_order BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_forum_categories_forum_slug ON forum_categories (forum_id, slug);
CREATE INDEX IF NOT EXISTS idx_forum_categories_forum ON forum_categories (forum_id, sort_order);

-- Threads: one discussion per row; the opening post is a forum_posts
-- row with depth 0 created together with the thread.
CREATE TABLE IF NOT EXISTS forum_threads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    forum_id UUID NOT NULL,
    category_id UUID,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    author_id TEXT NOT NULL CHECK (char_length(author_id) BETWEEN 1 AND 200),
    tags TEXT[] NOT NULL DEFAULT '{}',
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    locked BOOLEAN NOT NULL DEFAULT FALSE,
    solved BOOLEAN NOT NULL DEFAULT FALSE,
    best_answer_post_id UUID,
    view_count BIGINT NOT NULL DEFAULT 0 CHECK (view_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_forum_threads_solved_best_answer CHECK ((NOT solved OR best_answer_post_id IS NOT NULL) AND (best_answer_post_id IS NULL OR solved))
);
CREATE INDEX IF NOT EXISTS idx_forum_threads_forum ON forum_threads (forum_id, pinned, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_forum_threads_category ON forum_threads (category_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_forum_threads_author ON forum_threads (author_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_forum_threads_solved ON forum_threads (solved) WHERE solved;

-- Posts: replies nest at most two levels deep (depth 0/1/2).
CREATE TABLE IF NOT EXISTS forum_posts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    thread_id UUID NOT NULL,
    parent_post_id UUID,
    depth SMALLINT NOT NULL DEFAULT 0 CHECK (depth BETWEEN 0 AND 2),
    author_id TEXT NOT NULL CHECK (char_length(author_id) BETWEEN 1 AND 200),
    body TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 20000),
    hidden BOOLEAN NOT NULL DEFAULT FALSE,
    edited BOOLEAN NOT NULL DEFAULT FALSE,
    edited_at TIMESTAMPTZ,
    quoted_post_id UUID,
    attachment_ids TEXT[] NOT NULL DEFAULT '{}',
    mentions TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_forum_posts_thread ON forum_posts (thread_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_forum_posts_parent ON forum_posts (parent_post_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_forum_posts_author ON forum_posts (author_id, created_at DESC);

-- Kept edit history: the previous body, who edited it, and when.
CREATE TABLE IF NOT EXISTS forum_post_edits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    post_id UUID NOT NULL,
    editor_id TEXT NOT NULL CHECK (char_length(editor_id) BETWEEN 1 AND 200),
    previous_body TEXT NOT NULL,
    edited_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_forum_post_edits_post ON forum_post_edits (post_id, edited_at ASC);

-- Votes: +1 or -1, one row per (post, user).
CREATE TABLE IF NOT EXISTS forum_votes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    post_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    value SMALLINT NOT NULL CHECK (value IN (1, -1)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_forum_votes_post_user ON forum_votes (post_id, user_id);
CREATE INDEX IF NOT EXISTS idx_forum_votes_post ON forum_votes (post_id, value);

-- Reports: flags against a post or a thread, reviewed by moderators.
CREATE TABLE IF NOT EXISTS forum_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    target_type TEXT NOT NULL CHECK (target_type IN ('post', 'thread')),
    target_id UUID NOT NULL,
    reporter_id TEXT NOT NULL CHECK (char_length(reporter_id) BETWEEN 1 AND 200),
    reason TEXT NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 1000),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'dismissed', 'actioned')),
    reviewed_by TEXT,
    reviewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_forum_reports_status ON forum_reports (status, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_forum_reports_target ON forum_reports (target_type, target_id);

-- Audited moderator actions.
CREATE TABLE IF NOT EXISTS forum_moderation_actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    target_type TEXT NOT NULL,
    target_id UUID NOT NULL,
    action TEXT NOT NULL,
    moderator_id TEXT NOT NULL CHECK (char_length(moderator_id) BETWEEN 1 AND 200),
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_forum_moderation_actions_target ON forum_moderation_actions (target_type, target_id, created_at DESC);

-- Subscriptions: watch a thread or a whole forum.
CREATE TABLE IF NOT EXISTS forum_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    target_type TEXT NOT NULL CHECK (target_type IN ('thread', 'forum')),
    target_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_forum_subscriptions_user_target ON forum_subscriptions (tenant_id, user_id, target_type, target_id);
CREATE INDEX IF NOT EXISTS idx_forum_subscriptions_target ON forum_subscriptions (target_type, target_id);

-- Polls: at most one poll per thread; one vote per (poll, user).
CREATE TABLE IF NOT EXISTS forum_polls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    thread_id UUID NOT NULL,
    question TEXT NOT NULL CHECK (char_length(question) BETWEEN 1 AND 500),
    closed BOOLEAN NOT NULL DEFAULT FALSE,
    closed_at TIMESTAMPTZ,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_forum_polls_thread ON forum_polls (thread_id);

CREATE TABLE IF NOT EXISTS forum_poll_options (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    poll_id UUID NOT NULL,
    text TEXT NOT NULL CHECK (char_length(text) BETWEEN 1 AND 200),
    sort_order BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_forum_poll_options_poll ON forum_poll_options (poll_id, sort_order);

CREATE TABLE IF NOT EXISTS forum_poll_votes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    poll_id UUID NOT NULL,
    option_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_forum_poll_votes_poll_user ON forum_poll_votes (poll_id, user_id);
CREATE INDEX IF NOT EXISTS idx_forum_poll_votes_option ON forum_poll_votes (option_id);

-- Reputation: one row per (tenant, user); score is derived as
-- 10 * upvotes_received - 2 * downvotes_received + 15 * best_answers.
CREATE TABLE IF NOT EXISTS forum_reputation (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    score BIGINT NOT NULL DEFAULT 0,
    upvotes_received BIGINT NOT NULL DEFAULT 0 CHECK (upvotes_received >= 0),
    downvotes_received BIGINT NOT NULL DEFAULT 0 CHECK (downvotes_received >= 0),
    best_answers BIGINT NOT NULL DEFAULT 0 CHECK (best_answers >= 0),
    badge_count BIGINT NOT NULL DEFAULT 0 CHECK (badge_count >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_forum_reputation_tenant_user ON forum_reputation (tenant_id, user_id);
CREATE INDEX IF NOT EXISTS idx_forum_reputation_score ON forum_reputation (tenant_id, score DESC);

-- Per-tenant forum settings (rate limits, spam controls).
CREATE TABLE IF NOT EXISTS forum_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    max_threads_per_hour BIGINT NOT NULL DEFAULT 10 CHECK (max_threads_per_hour > 0),
    max_posts_per_hour BIGINT NOT NULL DEFAULT 30 CHECK (max_posts_per_hour > 0),
    spam_report_threshold BIGINT NOT NULL DEFAULT 5 CHECK (spam_report_threshold > 0),
    new_user_link_limit BIGINT NOT NULL DEFAULT 0 CHECK (new_user_link_limit >= 0),
    new_user_age_hours BIGINT NOT NULL DEFAULT 24 CHECK (new_user_age_hours >= 0),
    max_attachments_per_post BIGINT NOT NULL DEFAULT 5 CHECK (max_attachments_per_post >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
