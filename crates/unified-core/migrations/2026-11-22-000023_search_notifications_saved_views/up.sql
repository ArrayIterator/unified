-- Global search, notification centre, and saved-view tables for
-- the Unified Pentagonal Project (search.rs, notifications.rs,
-- saved_views.rs, timeline.rs): the searchable document index,
-- notifications with per-user preferences and templates, saved
-- filters / views, and the timeline entries.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `search_documents` is keyed by (kind, id): re-indexing the
--     same document replaces it instead of duplicating it,
--     exactly like the in-memory key `{kind}:{id}`. `kind` is one
--     of `software`, `product`, `order`, `invoice`, `license`,
--     `user`, `company`, `domain`, `task`, `article`, `ticket`,
--     `server`, `cluster`, or `backup`.
--   - `notifications.user_id` is VARCHAR: the notifications
--     module keeps user ids as strings, the same reservation as
--     the `store_orders.user_id` follow-up in docs/SCHEMA.md.
--     `updated_at` is added per the project convention that
--     every table carries both timestamps; the struct has only
--     `created_at`.
--   - `notification_preferences` is keyed by `user_id`: one
--     preference row per user. `channels` is a subset of
--     `in_app`, `email`, `webhook`; `categories` a subset of
--     `billing`, `security`, `sync`, `system`; `digest` is
--     `immediate`, `daily`, or `weekly`. `channel_by_category`
--     is the per-category channel override map.
--   - `saved_views` is the durable form of `SavedView`: one saved
--     filter per user per module. `filter` is a flat field/value
--     map (nested objects and nested arrays are rejected by the
--     module). At most one default view per (user, module) is
--     enforced by the partial unique index below; setting a
--     default clears the flag on the user's other views for that
--     module.
--   - `timeline_entries` is a materialized view of the event
--     stream: the timeline module subscribes to the records event
--     set plus the verified failure events (`security.auth.failed`,
--     `security.bruteforce.locked`, `cluster.sync.failed`,
--     `payment.failed`) and keeps the newest entries. `source` is
--     `record`, `audit`, `task`, `security`, `cluster`, or
--     `payment`. Rows are derivable from `system_records` and the
--     audit log, but the timeline keeps its own bounded cache
--     (5,000 entries in memory); this table is its durable form.
--     `seq` is a monotonic sequence number unique across the
--     table.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One searchable document in the global index.
CREATE TABLE IF NOT EXISTS search_documents (
    kind VARCHAR NOT NULL,
    id VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    snippet TEXT NOT NULL DEFAULT '',
    url VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, id)
);

CREATE INDEX IF NOT EXISTS idx_search_documents_kind
    ON search_documents (kind);
CREATE INDEX IF NOT EXISTS idx_search_documents_updated_at
    ON search_documents (updated_at);

-- One notification: an in-app / email / webhook message for a user.
CREATE TABLE IF NOT EXISTS notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    body TEXT NOT NULL,
    kind VARCHAR NOT NULL,
    read BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_notifications_user_id
    ON notifications (user_id);
CREATE INDEX IF NOT EXISTS idx_notifications_read
    ON notifications (read);
CREATE INDEX IF NOT EXISTS idx_notifications_created_at
    ON notifications (created_at);

-- Per-user notification preferences: channels, categories, and
-- digest cadence.
CREATE TABLE IF NOT EXISTS notification_preferences (
    user_id VARCHAR PRIMARY KEY,
    channels JSONB NOT NULL DEFAULT '["in_app", "email", "webhook"]'::jsonb,
    categories JSONB NOT NULL DEFAULT '["billing", "security", "sync", "system"]'::jsonb,
    channel_by_category JSONB NOT NULL DEFAULT '{}'::jsonb,
    digest VARCHAR NOT NULL DEFAULT 'immediate' CHECK (digest IN ('immediate', 'daily', 'weekly')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One notification template: per-event title and body templates
-- with `{{placeholders}}`, a locale, and the channels the event is
-- delivered through.
CREATE TABLE IF NOT EXISTS notification_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event VARCHAR NOT NULL,
    locale VARCHAR NOT NULL DEFAULT 'en',
    title_template TEXT NOT NULL,
    body_template TEXT NOT NULL,
    channels JSONB NOT NULL DEFAULT '["in_app"]'::jsonb,
    email_connection_id UUID REFERENCES email_connections (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_notification_templates_event_locale
    ON notification_templates (event, locale);
CREATE INDEX IF NOT EXISTS idx_notification_templates_event
    ON notification_templates (event);

-- One saved filter / view, per user and per module.
CREATE TABLE IF NOT EXISTS saved_views (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR NOT NULL,
    module_id VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    filter JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort VARCHAR,
    "order" VARCHAR,
    visibility VARCHAR NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'shared')),
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- At most one default view per (user, module).
CREATE UNIQUE INDEX IF NOT EXISTS uq_saved_views_user_module_default
    ON saved_views (user_id, module_id) WHERE is_default;
CREATE INDEX IF NOT EXISTS idx_saved_views_user_id
    ON saved_views (user_id);
CREATE INDEX IF NOT EXISTS idx_saved_views_module_id
    ON saved_views (module_id);

-- One timeline entry: a materialized event-stream row.
CREATE TABLE IF NOT EXISTS timeline_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    seq BIGINT NOT NULL,
    actor VARCHAR NOT NULL,
    action VARCHAR NOT NULL,
    entity_type VARCHAR NOT NULL,
    entity_id VARCHAR NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    source VARCHAR NOT NULL CHECK (source IN ('record', 'audit', 'task', 'security', 'cluster', 'payment')),
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    ip VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_timeline_entries_seq
    ON timeline_entries (seq);
CREATE INDEX IF NOT EXISTS idx_timeline_entries_action
    ON timeline_entries (action);
CREATE INDEX IF NOT EXISTS idx_timeline_entries_entity
    ON timeline_entries (entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_timeline_entries_source
    ON timeline_entries (source);
CREATE INDEX IF NOT EXISTS idx_timeline_entries_created_at
    ON timeline_entries (created_at);
