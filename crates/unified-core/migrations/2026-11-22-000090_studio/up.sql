-- Studio module tables for the Unified Pentagonal Project
-- (`src/modules/studio.rs`): metadata-driven user-side customizer.
-- Custom fields, validation rules, saved list views, and form
-- layouts are metadata only - this module never generates code and
-- never alters a table at runtime. Custom values live in the
-- generic key-value table `studio_field_values` (record_type +
-- record_id + field_key + typed JSON value), never as new columns.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the saved-views precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `company_id` is plain TEXT with an empty string meaning
--     "global" (never NULL), so unique indexes stay simple.
--     `record_id` / `user_id` / `created_by` are plain TEXT with NO
--     foreign key on purpose: users live in the users module and
--     this module must not assume its table or migration order.
--     `record_type` is plain TEXT for the same reason; the
--     registry in the module is the authority, not a foreign key.
--   - `studio_field_definitions` is versioned: an update stores a
--     new row with the same field_key and version + 1 and marks the
--     old row `superseded`. The unique index covers
--     (record_type, company_id, field_key, version).
--   - `studio_field_values` has a unique index on
--     (record_type, record_id, field_key, company_id): one stored
--     value per record, field, and company scope.
--   - `studio_form_layouts` has a partial unique index so at most
--     one default layout exists per (record_type, company_id).
--   - No secret is stored in any table here. `options`,
--     `validation`, `columns`, `filters`, and `sections` are JSONB
--     metadata; `filters` values are flat scalars only (enforced
--     by the module, never executable logic).
--   - Field types are a closed vocabulary in the module
--     (text / number / decimal / date / boolean / select /
--     user_reference); the CHECK constraint below mirrors it.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Record types: the admin-editable registry of record kinds a
-- customization may attach to (for example `crm.lead`).
CREATE TABLE IF NOT EXISTS studio_record_types (
    record_type TEXT PRIMARY KEY CHECK (char_length(record_type) BETWEEN 1 AND 128),
    module_id TEXT NOT NULL CHECK (char_length(module_id) BETWEEN 1 AND 64),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 500),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_studio_record_types_module ON studio_record_types (module_id);

-- System fields: the owning module's own fields for a record type.
-- `hideable = false` (or `required = true`) means a layout may
-- never hide the field.
CREATE TABLE IF NOT EXISTS studio_system_fields (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    record_type TEXT NOT NULL REFERENCES studio_record_types (record_type) ON DELETE CASCADE,
    field_key TEXT NOT NULL CHECK (char_length(field_key) BETWEEN 1 AND 64),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
    field_type TEXT NOT NULL CHECK (field_type IN ('text', 'number', 'decimal', 'date', 'boolean', 'select', 'user_reference')),
    required BOOLEAN NOT NULL DEFAULT FALSE,
    hideable BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (record_type, field_key)
);
CREATE INDEX IF NOT EXISTS idx_studio_system_fields_record ON studio_system_fields (record_type);

-- Custom field definitions (versioned). Custom keys must start with
-- the `x_` prefix (enforced by the module, not by the database:
-- the prefix is a configurable default, not a schema constant).
CREATE TABLE IF NOT EXISTS studio_field_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id TEXT NOT NULL DEFAULT '',
    record_type TEXT NOT NULL REFERENCES studio_record_types (record_type) ON DELETE CASCADE,
    field_key TEXT NOT NULL CHECK (char_length(field_key) BETWEEN 1 AND 64),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
    field_type TEXT NOT NULL CHECK (field_type IN ('text', 'number', 'decimal', 'date', 'boolean', 'select', 'user_reference')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'superseded', 'archived')),
    required BOOLEAN NOT NULL DEFAULT FALSE,
    default_value JSONB,
    help_text TEXT CHECK (help_text IS NULL OR char_length(help_text) BETWEEN 1 AND 500),
    options JSONB NOT NULL DEFAULT '[]'::jsonb,
    validation JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (record_type, company_id, field_key, version)
);
CREATE INDEX IF NOT EXISTS idx_studio_field_definitions_record ON studio_field_definitions (record_type, company_id, status);
CREATE INDEX IF NOT EXISTS idx_studio_field_definitions_key ON studio_field_definitions (field_key);

-- Custom values: the generic key-value store. One row per
-- (record, field, company scope); `value` is the typed JSON value
-- and `value_type` records the field type it was validated against.
CREATE TABLE IF NOT EXISTS studio_field_values (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id TEXT NOT NULL DEFAULT '',
    record_type TEXT NOT NULL,
    record_id TEXT NOT NULL CHECK (char_length(record_id) BETWEEN 1 AND 200),
    field_key TEXT NOT NULL CHECK (char_length(field_key) BETWEEN 1 AND 64),
    definition_id UUID REFERENCES studio_field_definitions (id) ON DELETE SET NULL,
    value_type TEXT NOT NULL CHECK (value_type IN ('text', 'number', 'decimal', 'date', 'boolean', 'select', 'user_reference')),
    value JSONB NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (record_type, record_id, field_key, company_id)
);
CREATE INDEX IF NOT EXISTS idx_studio_field_values_record ON studio_field_values (record_type, record_id);
CREATE INDEX IF NOT EXISTS idx_studio_field_values_field ON studio_field_values (field_key);

-- Saved list views: per user or per company, private or shared.
-- `scope` is `user` for private views and `company` for shared
-- views; it drives which default a view clears.
CREATE TABLE IF NOT EXISTS studio_list_views (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    company_id TEXT NOT NULL DEFAULT '',
    record_type TEXT NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    columns JSONB NOT NULL DEFAULT '[]'::jsonb,
    filters JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort_field TEXT CHECK (sort_field IS NULL OR char_length(sort_field) BETWEEN 1 AND 64),
    sort_order TEXT CHECK (sort_order IS NULL OR sort_order IN ('asc', 'desc')),
    group_by TEXT CHECK (group_by IS NULL OR char_length(group_by) BETWEEN 1 AND 64),
    visibility TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'shared')),
    scope TEXT NOT NULL DEFAULT 'user' CHECK (scope IN ('user', 'company')),
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_studio_list_views_record ON studio_list_views (record_type, company_id, user_id);
CREATE INDEX IF NOT EXISTS idx_studio_list_views_default ON studio_list_views (record_type, is_default);

-- Form layouts: field order and visibility per record type and
-- company, stored as metadata sections. At most one default layout
-- per (record_type, company_id).
CREATE TABLE IF NOT EXISTS studio_form_layouts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id TEXT NOT NULL DEFAULT '',
    record_type TEXT NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    sections JSONB NOT NULL DEFAULT '[]'::jsonb,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_studio_form_layouts_record ON studio_form_layouts (record_type, company_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_studio_form_layouts_one_default
    ON studio_form_layouts (record_type, company_id) WHERE is_default;
