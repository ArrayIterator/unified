-- 000133: content tags + dynamic custom post types (task 356).
--
-- Four tables, all tenant-scoped by `tenant_id` (the same partition
-- key the runtime stores carry; the tables are provisioned per
-- tenant schema, so the column also stays correct when a deployment
-- shares one schema across tenants):
--
--   * content_tags — the tags taxonomy: kebab-case slug, unique
--     per tenant. Shared by built-in posts and custom entries.
--   * content_post_tags — the post/tags join: one row per
--     (tenant, target_kind, target_id, tag_id). `target_kind` is
--     'post' for a built-in post or 'custom' for a custom entry.
--     Tags are never embedded on the post/entry row.
--   * content_post_types — admin-definable custom post type
--     definitions: labels, icon (module sprite symbol name),
--     `supports` (built-in features), `fields` (dynamic field
--     definitions, JSONB), visibility, archive/menu/REST flags,
--     `enabled`. Slug unique per tenant. Disabling a type hides
--     its routes/UI; its entries are preserved.
--   * content_custom_entries — ONE generic table for the entries of
--     EVERY custom type (no table per type). `type_slug` selects
--     the type; `fields` is the JSONB map of validated dynamic
--     field values, keyed by field key.
--
-- The runtime (src/modules/content) is in-memory today, like the
-- rest of the content module; these tables are the durable shape
-- the persistence follow-up writes to, and the Diesel schema in
-- crates/unified-db-platform/src/schema/content.rs mirrors them
-- exactly. No migration has ever run on live PostgreSQL; every
-- statement is IF NOT EXISTS, so a re-run is a no-op.

CREATE TABLE IF NOT EXISTS content_tags (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    description TEXT CHECK (description IS NULL OR char_length(description) <= 1000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_content_tags_tenant_slug
    ON content_tags (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_content_tags_tenant_name
    ON content_tags (tenant_id, name);

CREATE TABLE IF NOT EXISTS content_post_tags (
    tenant_id UUID NOT NULL,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('post', 'custom')),
    target_id UUID NOT NULL,
    tag_id UUID NOT NULL REFERENCES content_tags (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, target_kind, target_id, tag_id)
);

CREATE INDEX IF NOT EXISTS idx_content_post_tags_tag
    ON content_post_tags (tenant_id, tag_id);
CREATE INDEX IF NOT EXISTS idx_content_post_tags_target
    ON content_post_tags (tenant_id, target_kind, target_id);

CREATE TABLE IF NOT EXISTS content_post_types (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    singular_label TEXT NOT NULL CHECK (char_length(singular_label) BETWEEN 1 AND 200),
    plural_label TEXT NOT NULL CHECK (char_length(plural_label) BETWEEN 1 AND 200),
    icon TEXT NOT NULL DEFAULT 'document' CHECK (char_length(icon) BETWEEN 1 AND 64),
    description TEXT CHECK (description IS NULL OR char_length(description) <= 1000),
    supports JSONB NOT NULL DEFAULT '[]'::jsonb,
    fields JSONB NOT NULL DEFAULT '[]'::jsonb,
    visibility TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('public', 'private')),
    has_archive BOOLEAN NOT NULL DEFAULT FALSE,
    show_in_menu BOOLEAN NOT NULL DEFAULT FALSE,
    menu_position INTEGER NOT NULL DEFAULT 0,
    rest_exposed BOOLEAN NOT NULL DEFAULT TRUE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_content_post_types_tenant_slug
    ON content_post_types (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_content_post_types_tenant_enabled
    ON content_post_types (tenant_id, enabled);

CREATE TABLE IF NOT EXISTS content_custom_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    type_slug TEXT NOT NULL CHECK (char_length(type_slug) BETWEEN 1 AND 64),
    title TEXT NOT NULL DEFAULT '' CHECK (char_length(title) <= 500),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    content TEXT NOT NULL DEFAULT '' CHECK (char_length(content) <= 1000000),
    excerpt TEXT NOT NULL DEFAULT '' CHECK (char_length(excerpt) <= 300),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    category_id UUID,
    author_id UUID,
    thumbnail TEXT CHECK (thumbnail IS NULL OR char_length(thumbnail) <= 500),
    fields JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_content_custom_entries_tenant_type_slug
    ON content_custom_entries (tenant_id, type_slug, slug);
CREATE INDEX IF NOT EXISTS idx_content_custom_entries_tenant_type_status
    ON content_custom_entries (tenant_id, type_slug, status);
CREATE INDEX IF NOT EXISTS idx_content_custom_entries_tenant_category
    ON content_custom_entries (tenant_id, category_id);
CREATE INDEX IF NOT EXISTS idx_content_custom_entries_fields
    ON content_custom_entries USING GIN (fields);
