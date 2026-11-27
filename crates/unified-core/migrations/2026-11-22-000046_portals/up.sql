-- Client Portal (white-label) tables for the Unified Pentagonal Project
-- (`src/modules/portals.rs`): branded portals per company, portal
-- menu entries, and recorded portal page views.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/portals.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `portals.company_id` and `portals.created_by` are plain
--     VARCHAR references: the module identifies callers by the
--     `X-User-Id` header and keeps company / user ids as strings,
--     exactly like `store_orders.user_id` in migration 000002. No
--     foreign key to `users` or `companies` is stored here.
--   - `portals.slug` is unique (lowercase letters, digits, single
--     hyphens; validated in the module). `portals.custom_domain`
--     is unique when set: one domain serves one portal only.
--   - `portals.domain_status` is `none`, `requested`, `verified`,
--     or `rejected`. Only the domains module moves it past
--     `requested` (through its listener on
--     `portals.domain.requested`); the portals module never
--     provisions or verifies a domain itself.
--   - `portals.logo_storage_target` / `portals.logo_storage_key`
--     are metadata only: the logo bytes live in the storage
--     module, referenced by target + key (same shape as the events
--     module cover image). The CHECK keeps the pair together.
--   - Brand colours are `#RGB` / `#RRGGBB` hex, enforced by the
--     CHECK below and validated in the module.
--   - `portals.sections` is a TEXT array of visible sections in
--     display order; every entry is one of `invoices`, `tickets`,
--     `downloads`, `subscriptions`, `licenses` (enforced by the
--     array-containment CHECK below). Duplicates are rejected by
--     the module before write; a CHECK cannot test for duplicates
--     because PostgreSQL forbids subqueries in CHECK constraints.
--   - `portals.status` is `draft`, `active`, or `disabled`
--     (`PortalStatus`). Only `active` portals answer public and
--     client routes.
--   - `portal_menus.target_kind` is `section` (target is a section
--     name from the list above) or `url` (target is an `http://` /
--     `https://` URL). `roles` limits the entry to client roles;
--     an empty array means every client role. `position` orders
--     entries inside one portal (unique per portal).
--   - `portal_views` records one row per public portal render or
--     client home visit: viewer id when authenticated, client IP,
--     and user agent. No secrets are stored. The module caps views
--     per portal at 10,000 rows in memory (oldest dropped first);
--     the same bound should be applied by any retention job on
--     this table.
--   - The read-only client-data projection (`PortalRecord`: my
--     invoices, tickets, downloads, subscriptions, licenses) has
--     NO table here on purpose: those records belong to the
--     billing / tickets / store / licensing modules, which own
--     their tables. The projection is rebuilt from their hook
--     events at runtime and is never written back.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One branded client portal, owned by one company.
CREATE TABLE IF NOT EXISTS portals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR(128) NOT NULL,
    name VARCHAR(200) NOT NULL,
    slug VARCHAR(128) NOT NULL,
    custom_domain VARCHAR(253),
    domain_status VARCHAR NOT NULL DEFAULT 'none'
        CHECK (domain_status IN ('none', 'requested', 'verified', 'rejected')),
    logo_storage_target VARCHAR(512),
    logo_storage_key VARCHAR(512),
    primary_color VARCHAR(7) NOT NULL DEFAULT '#0B6B3A',
    secondary_color VARCHAR(7) NOT NULL DEFAULT '#FFFFFF',
    accent_color VARCHAR(7) NOT NULL DEFAULT '#C9A227',
    background_color VARCHAR(7) NOT NULL DEFAULT '#FFFFFF',
    text_color VARCHAR(7) NOT NULL DEFAULT '#1A1A1A',
    sections TEXT[] NOT NULL DEFAULT ARRAY['invoices', 'tickets', 'downloads', 'subscriptions', 'licenses'],
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'disabled')),
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((logo_storage_target IS NULL) = (logo_storage_key IS NULL)),
    CHECK (primary_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (secondary_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (accent_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (background_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (text_color ~ '^#[0-9A-Fa-f]{3}([0-9A-Fa-f]{3})?$'),
    CHECK (sections <@ ARRAY['invoices', 'tickets', 'downloads', 'subscriptions', 'licenses']),
    CHECK ((custom_domain IS NULL) = (domain_status = 'none') OR (custom_domain IS NOT NULL AND domain_status <> 'none'))
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_portals_slug ON portals (slug);
CREATE UNIQUE INDEX IF NOT EXISTS uq_portals_custom_domain ON portals (custom_domain);
CREATE INDEX IF NOT EXISTS idx_portals_company_id ON portals (company_id);
CREATE INDEX IF NOT EXISTS idx_portals_status ON portals (status);

-- One menu entry of a portal.
CREATE TABLE IF NOT EXISTS portal_menus (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    portal_id UUID NOT NULL REFERENCES portals (id) ON DELETE CASCADE,
    label VARCHAR(128) NOT NULL,
    target_kind VARCHAR NOT NULL DEFAULT 'section'
        CHECK (target_kind IN ('section', 'url')),
    target VARCHAR(2048) NOT NULL,
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    visible BOOLEAN NOT NULL DEFAULT TRUE,
    roles TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (target_kind <> 'section' OR target = ANY (ARRAY['invoices', 'tickets', 'downloads', 'subscriptions', 'licenses'])),
    CHECK (target_kind <> 'url' OR target LIKE 'http://%' OR target LIKE 'https://%')
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_portal_menus_portal_position
    ON portal_menus (portal_id, position);
CREATE INDEX IF NOT EXISTS idx_portal_menus_portal_id ON portal_menus (portal_id);

-- One recorded portal page view.
CREATE TABLE IF NOT EXISTS portal_views (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    portal_id UUID NOT NULL REFERENCES portals (id) ON DELETE CASCADE,
    slug VARCHAR(128) NOT NULL,
    viewer_user_id VARCHAR,
    ip VARCHAR(45),
    user_agent VARCHAR(512),
    viewed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_portal_views_portal_id ON portal_views (portal_id);
CREATE INDEX IF NOT EXISTS idx_portal_views_viewed_at ON portal_views (viewed_at);
CREATE INDEX IF NOT EXISTS idx_portal_views_viewer_user_id ON portal_views (viewer_user_id);
