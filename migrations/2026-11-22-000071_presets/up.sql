-- Presets + Setup Wizard module tables for the Unified Pentagonal Project
-- (`src/modules/presets.rs`): admin-editable preset definitions, their
-- module entries, per-tenant wizard state, and preset application
-- records. Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` / actor columns are TEXT with NO foreign key on
--     purpose: actor ids in this codebase are opaque strings (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table. The users module stays the source of
--     truth for identity; preset rows only reference actors by id
--     and never duplicate user rows.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE: deleting a company
--     removes its wizard state and its application records.
--   - JSON columns are JSONB: `presets.settings`,
--     `presets.steps`, `preset_wizard_states.settings`,
--     `preset_wizard_states.disabled_optional_modules`,
--     `preset_wizard_states.skipped_steps`,
--     `preset_applications.outcomes`,
--     `preset_applications.settings_applied`.
--   - No money columns here. All ids are UUID, all timestamps are
--     Timestamptz.
--   - The four built-in presets are seeded below as data rows with
--     the same ids as the in-memory seeds in `src/modules/presets.rs`
--     (`builtin_preset_seeds`). Presets are data: an admin edits them
--     through the API (built-ins by cloning), never by editing code.
--     Every module id in the seed rows was verified against the
--     registered module list in `src/modules/mod.rs`; module ids
--     that exist only as unregistered files on disk (properties,
--     web3, timetracker, todos, portals, workflows, network_tools)
--     are NOT seeded, and there is no "invoices" module (billing is
--     the real billing/invoice module).
--   - `presets.steps` holds the wizard step sequence for the preset
--     as data, in order. The step names come from the closed step
--     vocabulary in the module; an empty list means "use the
--     default sequence" at wizard start.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One preset definition: a named, admin-editable bundle of modules
-- for one business type, plus the settings to apply on activation
-- and the wizard step sequence for that preset. `built_in` marks the
-- four seeded rows; they are edited by cloning, never in place.
-- `archived` presets are hidden by default and cannot be applied.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS presets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    business_type TEXT NOT NULL,
    built_in BOOLEAN NOT NULL DEFAULT FALSE,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_by TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS presets_business_type_idx ON presets (business_type);
CREATE INDEX IF NOT EXISTS presets_archived_idx ON presets (archived);

-- ---------------------------------------------------------------------------
-- One module entry inside a preset: the module id, whether it is
-- required (locked on in the wizard review step) or optional, and
-- its display order. One module per preset at most.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS preset_modules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    preset_id UUID NOT NULL REFERENCES presets (id) ON DELETE CASCADE,
    module_id TEXT NOT NULL,
    required BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (preset_id, module_id)
);

CREATE INDEX IF NOT EXISTS preset_modules_preset_idx ON preset_modules (preset_id);
CREATE INDEX IF NOT EXISTS preset_modules_module_idx ON preset_modules (module_id);

-- ---------------------------------------------------------------------------
-- Setup wizard state for one tenant: the chosen preset, the stored
-- step sequence (copied from the preset, or the default), the
-- current step, the steps skipped so far, the optional modules
-- toggled off, the settings answers, and the application produced
-- when the preset was applied. One wizard per company.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS preset_wizard_states (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    actor_id TEXT NOT NULL DEFAULT '',
    preset_id UUID REFERENCES presets (id) ON DELETE SET NULL,
    steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    current_step TEXT NOT NULL DEFAULT 'business_type',
    status TEXT NOT NULL DEFAULT 'in_progress',
    skipped_steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    disabled_optional_modules JSONB NOT NULL DEFAULT '[]'::jsonb,
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    application_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (company_id)
);

CREATE INDEX IF NOT EXISTS preset_wizard_states_company_idx ON preset_wizard_states (company_id);
CREATE INDEX IF NOT EXISTS preset_wizard_states_status_idx ON preset_wizard_states (status);

-- ---------------------------------------------------------------------------
-- One preset application: the recorded result of applying a preset
-- to a tenant. `outcomes` is the per-module result list (enabled,
-- already_enabled, skipped_not_entitled,
-- skipped_dependency_missing, skipped_optional_off, failed, each
-- with an optional reason and auto-activated dependencies).
-- `status` is completed, partial, or failed.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS preset_applications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    preset_id UUID REFERENCES presets (id) ON DELETE SET NULL,
    preset_slug TEXT NOT NULL,
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    actor_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'completed',
    outcomes JSONB NOT NULL DEFAULT '[]'::jsonb,
    settings_applied JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS preset_applications_preset_idx ON preset_applications (preset_id);
CREATE INDEX IF NOT EXISTS preset_applications_company_idx ON preset_applications (company_id);
CREATE INDEX IF NOT EXISTS preset_applications_status_idx ON preset_applications (status);
CREATE INDEX IF NOT EXISTS preset_applications_created_idx ON preset_applications (created_at DESC);

-- ---------------------------------------------------------------------------
-- Seed data: the four built-in presets, with the same ids as the
-- in-memory seeds in `src/modules/presets.rs`. Inserted with
-- ON CONFLICT DO NOTHING so re-running the migration never
-- duplicates or overwrites admin edits to these rows.
-- ---------------------------------------------------------------------------
INSERT INTO presets (id, slug, name, description, business_type, built_in, archived, settings, steps)
VALUES
    ('10000000-0000-4000-8000-000000000001', 'property-seller', 'Property Seller',
     'For businesses selling property: customer relations, billing, and notifications.',
     'property_seller', TRUE, FALSE, '{}'::jsonb,
     '["business_type", "review_modules", "apply", "settings", "done"]'::jsonb),
    ('10000000-0000-4000-8000-000000000002', 'freelancer', 'Freelancer',
     'For independent professionals: projects, billing, and task tracking.',
     'freelancer', TRUE, FALSE, '{}'::jsonb,
     '["business_type", "review_modules", "apply", "settings", "done"]'::jsonb),
    ('10000000-0000-4000-8000-000000000003', 'agency', 'Agency',
     'For agencies: projects, customer relations, and support tickets.',
     'agency', TRUE, FALSE, '{}'::jsonb,
     '["business_type", "review_modules", "apply", "settings", "done"]'::jsonb),
    ('10000000-0000-4000-8000-000000000004', 'saas-seller', 'SaaS Seller',
     'For software sellers: store, licensing, billing, and the public API.',
     'saas_seller', TRUE, FALSE, '{}'::jsonb,
     '["business_type", "review_modules", "apply", "settings", "done"]'::jsonb)
ON CONFLICT (id) DO NOTHING;

INSERT INTO preset_modules (preset_id, module_id, required, sort_order)
VALUES
    ('10000000-0000-4000-8000-000000000001', 'crm', TRUE, 0),
    ('10000000-0000-4000-8000-000000000001', 'billing', TRUE, 1),
    ('10000000-0000-4000-8000-000000000001', 'notifications', FALSE, 2),
    ('10000000-0000-4000-8000-000000000002', 'projects', TRUE, 0),
    ('10000000-0000-4000-8000-000000000002', 'billing', TRUE, 1),
    ('10000000-0000-4000-8000-000000000002', 'tasks', FALSE, 2),
    ('10000000-0000-4000-8000-000000000003', 'projects', TRUE, 0),
    ('10000000-0000-4000-8000-000000000003', 'crm', TRUE, 1),
    ('10000000-0000-4000-8000-000000000003', 'tickets', FALSE, 2),
    ('10000000-0000-4000-8000-000000000004', 'store', TRUE, 0),
    ('10000000-0000-4000-8000-000000000004', 'licensing', TRUE, 1),
    ('10000000-0000-4000-8000-000000000004', 'billing', TRUE, 2),
    ('10000000-0000-4000-8000-000000000004', 'api_manager', FALSE, 3)
ON CONFLICT (preset_id, module_id) DO NOTHING;
