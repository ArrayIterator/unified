-- Customizable page-route tables for the Unified Pentagonal
-- Project (routes.rs, `crate::core::routes`): the named routes
-- with their override layers, and the active old-path redirects.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the `RouteRegistry` in-memory store is
-- still authoritative; see TODO.md):
--   - `named_routes` is keyed by `name`: one row per built-in
--     named route (`login`, `register`, `forgot_password`,
--     `reset_password`, `logout`, `user_dashboard`,
--     `admin_dashboard`, `status_page`). `default_path` is the
--     built-in default; `config_override` and
--     `settings_override` are the two override layers (config
--     file, then admin settings); `source` records which layer is
--     currently in force (`none`, `config`, or `settings`).
--     `previous_path` records the path before the last change, so
--     the admin UI can show what clearing an override falls back
--     to.
--   - `route_redirects` is keyed by `route`: at most one active
--     old-path redirect exists per route name (latest-only).
--   - The admin and user dashboard paths are the configurable
--     defaults from the route-registry extension (config <
--     admin settings precedence); `/admin/` is only the default
--     for `admin_dashboard`.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One named route with its default path and override layers.
CREATE TABLE IF NOT EXISTS named_routes (
    name VARCHAR PRIMARY KEY,
    default_path VARCHAR NOT NULL,
    current_path VARCHAR NOT NULL,
    source VARCHAR NOT NULL DEFAULT 'none' CHECK (source IN ('none', 'config', 'settings')),
    description TEXT NOT NULL DEFAULT '',
    config_override VARCHAR,
    settings_override VARCHAR,
    previous_path VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One active old-path redirect (latest-only per route).
CREATE TABLE IF NOT EXISTS route_redirects (
    route VARCHAR PRIMARY KEY REFERENCES named_routes (name) ON DELETE CASCADE ON UPDATE NO ACTION,
    old_path VARCHAR NOT NULL,
    new_path VARCHAR NOT NULL,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_route_redirects_old_path
    ON route_redirects (old_path);
