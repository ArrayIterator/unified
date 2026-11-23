-- Widget layouts (task 352): one durable row per (context, scope, owner).
--
-- The widgets module is DB-backed by design: layouts are read from
-- this table on every request and written here on every save. There
-- is no in-memory layout cache and no DashMap authority (the C11
-- rule); the settings KV store is never used for layouts because it
-- is DashMap-only and lost on restart.
--
-- Key:
--   context    one of system-home, admin-dashboard,
--              tenant-dashboard, user-dashboard.
--   scope_kind `platform` for control-plane contexts (system-home,
--              admin-dashboard, user-dashboard) and `tenant` for
--              tenant-dashboard.
--   scope_id   `platform` for platform scope, the tenant/company id
--              for tenant scope.
--   owner_id   empty string for the context DEFAULT layout, a user id
--              for that user's override. A missing row means "use the
--              built-in default for the context" (reads never fail
--              for a missing row; only writes need the database).
-- items is a JSON array of {widget_id, pinned} in display order;
-- the array order IS the layout order (no separate sort column).

CREATE TABLE IF NOT EXISTS widget_layouts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    context TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    owner_id TEXT NOT NULL DEFAULT '',
    items JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_widget_layouts_key
    ON widget_layouts (context, scope_kind, scope_id, owner_id);
CREATE INDEX IF NOT EXISTS idx_widget_layouts_context_scope
    ON widget_layouts (context, scope_kind, scope_id);
