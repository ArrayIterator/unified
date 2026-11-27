-- White Labelling module tables for the Unified Pentagonal Project
-- (`src/modules/whitelabel.rs`): global + per-company allowance,
-- admin-editable global defaults record, tenant branding profiles,
-- and the branding change history (including denied attempts).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/whitelabel.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `whitelabel_global_settings` holds exactly one row
--     (`id = 'global'`): the global default allowance
--     (`allow_whitelabel`, default FALSE - fail closed).
--   - `whitelabel_company_settings.mode` is `allowed`, `denied`, or
--     `inherit` (inherit follows the global default). `company_id`
--     is a plain VARCHAR reference: the module identifies callers
--     by the `X-User-Id` / `X-Company-Id` headers and keeps company
--     ids as strings, exactly like `portals.company_id` in
--     migration 000046. No foreign key to `users` or `companies`
--     is stored here.
--   - `whitelabel_global_defaults` holds exactly one row
--     (`id = 'global'`): the admin-editable platform defaults
--     record (default logo, colours, sender, product name, login
--     tagline, "Powered by Unified" text). The seed values in
--     `GlobalDefaults::seed()` are only the initial contents of
--     this row on a fresh install; once an admin saves defaults,
--     this row is the source of truth. No branding default is
--     hard-coded anywhere else.
--   - `whitelabel_profiles` holds one profile per
--     `(company_id, tenant_id)` pair. A NULL `tenant_id` is the
--     company default profile; a set `tenant_id` is that tenant's
--     override inside the company. The expression unique index
--     below enforces one row per pair (a plain UNIQUE constraint
--     would treat NULLs as distinct and allow duplicate company
--     defaults).
--   - Logo / favicon / login images are storage references only
--     (storage target + key, same shape as the portals module
--     logo): the bytes live in the storage module. The CHECKs keep
--     each target/key pair together.
--   - Brand colours are `#RGB` / `#RRGGBB` hex, enforced by the
--     CHECKs below and validated in the module.
--   - `whitelabel_profiles.custom_domain` is unique when set: one
--     domain serves one profile only. `domain_status` is `none`,
--     `requested`, `verified`, or `rejected` (the same set the
--     portals module uses). Only the domains module moves it past
--     `requested` (through its `domain.verify` event); this module
--     never verifies a domain itself.
--   - `whitelabel_history` records every branding change AND every
--     disallowed attempt (`allowed = false`). `actor` is the
--     `X-User-Id` of the caller, or `system:domains` for
--     verification outcomes applied from the domains module.
--     `old_value` / `new_value` are plain strings, never secrets:
--     this module stores no secrets.
--   - `company_id` / `tenant_id` columns are plain VARCHAR
--     references with NO foreign key on purpose, matching the
--     portals migration: actor and tenant ids in this codebase are
--     opaque strings, not necessarily UUID primary keys.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Global default allowance: exactly one row (id = 'global').
CREATE TABLE IF NOT EXISTS whitelabel_global_settings (
    id VARCHAR(64) PRIMARY KEY,
    allow_whitelabel BOOLEAN NOT NULL DEFAULT FALSE,
    updated_by VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (id = 'global')
);

-- Per-company allowance override: allowed / denied / inherit.
CREATE TABLE IF NOT EXISTS whitelabel_company_settings (
    company_id VARCHAR(128) PRIMARY KEY,
    mode VARCHAR(16) NOT NULL DEFAULT 'inherit',
    updated_by VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (mode IN ('allowed', 'denied', 'inherit'))
);

-- Admin-editable platform defaults record: exactly one row
-- (id = 'global'). Source of truth for every default brand value.
CREATE TABLE IF NOT EXISTS whitelabel_global_defaults (
    id VARCHAR(64) PRIMARY KEY,
    display_name VARCHAR(200) NOT NULL DEFAULT 'Unified',
    logo_storage_target VARCHAR(512),
    logo_storage_key VARCHAR(512),
    favicon_storage_target VARCHAR(512),
    favicon_storage_key VARCHAR(512),
    primary_color VARCHAR(7) NOT NULL DEFAULT '#2563EB',
    accent_color VARCHAR(7) NOT NULL DEFAULT '#0EA5E9',
    background_color VARCHAR(7) NOT NULL DEFAULT '#FFFFFF',
    text_color VARCHAR(7) NOT NULL DEFAULT '#111827',
    sender_name VARCHAR(200) NOT NULL DEFAULT 'Unified',
    sender_email VARCHAR(320) NOT NULL DEFAULT '',
    login_tagline VARCHAR(300) NOT NULL DEFAULT 'Manage your business in one place.',
    login_background_storage_target VARCHAR(512),
    login_background_storage_key VARCHAR(512),
    powered_by_text VARCHAR(200) NOT NULL DEFAULT 'Powered by Unified',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (id = 'global'),
    CHECK (primary_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (accent_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (background_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (text_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK ((logo_storage_target IS NULL) = (logo_storage_key IS NULL)),
    CHECK ((favicon_storage_target IS NULL) = (favicon_storage_key IS NULL)),
    CHECK ((login_background_storage_target IS NULL) = (login_background_storage_key IS NULL))
);

-- One tenant branding profile per (company_id, tenant_id). NULL
-- tenant_id is the company default profile.
CREATE TABLE IF NOT EXISTS whitelabel_profiles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR(128) NOT NULL,
    tenant_id VARCHAR(128),
    display_name VARCHAR(200),
    logo_storage_target VARCHAR(512),
    logo_storage_key VARCHAR(512),
    favicon_storage_target VARCHAR(512),
    favicon_storage_key VARCHAR(512),
    primary_color VARCHAR(7),
    accent_color VARCHAR(7),
    background_color VARCHAR(7),
    text_color VARCHAR(7),
    custom_domain VARCHAR(253),
    domain_status VARCHAR(16) NOT NULL DEFAULT 'none',
    hide_powered_by BOOLEAN NOT NULL DEFAULT FALSE,
    sender_name VARCHAR(200),
    sender_email VARCHAR(320),
    login_logo_storage_target VARCHAR(512),
    login_logo_storage_key VARCHAR(512),
    login_background_storage_target VARCHAR(512),
    login_background_storage_key VARCHAR(512),
    login_tagline VARCHAR(300),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (domain_status IN ('none', 'requested', 'verified', 'rejected')),
    CHECK (primary_color IS NULL OR primary_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (accent_color IS NULL OR accent_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (background_color IS NULL OR background_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (text_color IS NULL OR text_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK ((logo_storage_target IS NULL) = (logo_storage_key IS NULL)),
    CHECK ((favicon_storage_target IS NULL) = (favicon_storage_key IS NULL)),
    CHECK ((login_logo_storage_target IS NULL) = (login_logo_storage_key IS NULL)),
    CHECK ((login_background_storage_target IS NULL) = (login_background_storage_key IS NULL))
);

-- One profile per (company_id, tenant_id); NULL tenant_id counts as
-- the company default (COALESCE makes the pair unique).
CREATE UNIQUE INDEX IF NOT EXISTS whitelabel_profiles_company_tenant_idx
    ON whitelabel_profiles (company_id, COALESCE(tenant_id, ''));

-- One domain serves one profile only.
CREATE UNIQUE INDEX IF NOT EXISTS whitelabel_profiles_custom_domain_idx
    ON whitelabel_profiles (custom_domain) WHERE custom_domain IS NOT NULL;

CREATE INDEX IF NOT EXISTS whitelabel_profiles_company_idx
    ON whitelabel_profiles (company_id);

-- Branding change history, including denied attempts (allowed=false).
CREATE TABLE IF NOT EXISTS whitelabel_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR(128) NOT NULL,
    tenant_id VARCHAR(128),
    actor VARCHAR(128) NOT NULL,
    action VARCHAR(64) NOT NULL,
    field VARCHAR(64) NOT NULL,
    old_value TEXT,
    new_value TEXT,
    allowed BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS whitelabel_history_company_idx
    ON whitelabel_history (company_id, created_at DESC);

CREATE INDEX IF NOT EXISTS whitelabel_history_action_idx
    ON whitelabel_history (action, created_at DESC);
