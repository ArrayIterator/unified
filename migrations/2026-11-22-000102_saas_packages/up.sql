-- SaaS Packages module tables for the Unified Pentagonal Project
-- (`src/modules/saas_packages.rs`): package definitions, versioned
-- package module lists, package purchases, and provisioning steps.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs). This migration has never been executed against a
-- live PostgreSQL database.
--
-- Notes on the mapping (the in-memory DashMap stores are still
-- authoritative; see the e-learning precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path.
--   - `saas_packages`, `saas_package_modules`, and
--     `saas_package_purchases` are CONTROL-PLANE data: they live in
--     the `public` schema with the other control-plane tables
--     (tenants, subscriptions, domains, billing, licensing).
--     The tenant's enabled-module state (the in-memory
--     `tenant_modules` map, keyed "<company_id>|<module_id>") is
--     TENANT data: at persistence time it moves into the tenant
--     schema derived with `tenancy::schema_name()`, never into
--     `public`. No tenant id, schema name, or package id is
--     hard-coded anywhere in the module.
--   - All ids are plain TEXT with NO foreign key on purpose:
--     users, companies, products, and orders live in other modules
--     and this module must not assume their tables or migration
--     order. `product_id` is only a reference to a product/plan in
--     the products module, which owns pricing; NO money is stored
--     in any table here (no NUMERIC column exists).
--   - `saas_packages` holds the CURRENT version of each package
--     only. The versioned module lists live in
--     `saas_package_modules` (one row per package, version, and
--     module), so the module list of any past version can be
--     rebuilt; the frozen name/description/settings/steps snapshot
--     of a past version (the in-memory `PackageVersion`) is NOT
--     persisted by this migration — the integrator adds a versions
--     table if durable snapshots are needed.
--   - `saas_package_purchases.enabled_modules` is JSONB: the
--     module ids the purchase enabled for the tenant.
--   - `saas_package_provision_steps.detail` is free text, except
--     for the `enable_modules` step, where it carries the
--     per-module outcomes as a JSON array (see the module code).
--     No secret is stored in any table here: provisioning steps
--     never carry credentials (the welcome step references the
--     tenant link only).
--   - `saas_package_purchases.status` and
--     `saas_package_provision_steps.status` are closed vocabularies
--     enforced by CHECK; the allowed transitions are enforced
--     server-side in `src/modules/saas_packages.rs`, the database
--     only guards the vocabulary, never the graph.
--   - The seed INSERTs at the bottom are the example "Online Exam
--     SaaS" package from the module (`SEED_ONLINE_EXAM_PACKAGE_ID`
--     `20000000-0000-4000-8000-000000000001`), with the same id,
--     slug, name, description, settings, wizard steps, and module
--     list, so the database and the in-memory store agree. It is
--     example data the admin can edit or delete like any other
--     package.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Package definitions (control plane).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS saas_packages (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE CHECK (char_length(slug) BETWEEN 1 AND 100),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT NOT NULL CHECK (char_length(description) BETWEEN 1 AND 5000),
    status TEXT NOT NULL CHECK (status IN ('draft', 'published', 'archived')),
    current_version INTEGER NOT NULL DEFAULT 1 CHECK (current_version >= 1),
    product_id TEXT CHECK (product_id IS NULL OR char_length(product_id) BETWEEN 1 AND 500),
    preset_settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    wizard_steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 500),
    vendor_id TEXT CHECK (vendor_id IS NULL OR char_length(vendor_id) BETWEEN 1 AND 500),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_saas_packages_status ON saas_packages (status);
CREATE INDEX IF NOT EXISTS idx_saas_packages_vendor ON saas_packages (vendor_id);

-- ---------------------------------------------------------------------------
-- Versioned module lists of a package. One row per package, version,
-- and module; the primary key keeps a version's list exact.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS saas_package_modules (
    package_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    module_id TEXT NOT NULL CHECK (char_length(module_id) BETWEEN 1 AND 128),
    required BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (package_id, version, module_id)
);
CREATE INDEX IF NOT EXISTS idx_saas_package_modules_package ON saas_package_modules (package_id, version);

-- ---------------------------------------------------------------------------
-- Purchases of a package by a buyer for a tenant (company). A
-- purchase pins the package version it bought.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS saas_package_purchases (
    id TEXT PRIMARY KEY,
    package_id TEXT NOT NULL,
    package_version INTEGER NOT NULL CHECK (package_version >= 1),
    buyer_id TEXT NOT NULL CHECK (char_length(buyer_id) BETWEEN 1 AND 500),
    company_id TEXT NOT NULL CHECK (char_length(company_id) BETWEEN 1 AND 500),
    status TEXT NOT NULL CHECK (status IN ('pending_payment', 'provisioning', 'active', 'failed', 'suspended', 'grace_period', 'cancelled', 'expired')),
    subdomain TEXT CHECK (subdomain IS NULL OR char_length(subdomain) BETWEEN 1 AND 63),
    subdomain_pending BOOLEAN NOT NULL DEFAULT FALSE,
    order_id TEXT CHECK (order_id IS NULL OR char_length(order_id) BETWEEN 1 AND 500),
    subscription_id TEXT CHECK (subscription_id IS NULL OR char_length(subscription_id) BETWEEN 1 AND 500),
    product_id TEXT CHECK (product_id IS NULL OR char_length(product_id) BETWEEN 1 AND 500),
    enabled_modules JSONB NOT NULL DEFAULT '[]'::jsonb,
    grace_ends_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    cancel_reason TEXT CHECK (cancel_reason IS NULL OR char_length(cancel_reason) BETWEEN 1 AND 20000),
    provision_attempts INTEGER NOT NULL DEFAULT 0 CHECK (provision_attempts >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_saas_package_purchases_package ON saas_package_purchases (package_id);
CREATE INDEX IF NOT EXISTS idx_saas_package_purchases_company ON saas_package_purchases (company_id);
CREATE INDEX IF NOT EXISTS idx_saas_package_purchases_status ON saas_package_purchases (status);
CREATE INDEX IF NOT EXISTS idx_saas_package_purchases_order ON saas_package_purchases (order_id);
CREATE INDEX IF NOT EXISTS idx_saas_package_purchases_buyer ON saas_package_purchases (buyer_id);

-- ---------------------------------------------------------------------------
-- Provisioning steps. One row per purchase, step, and attempt, so the
-- full provisioning history is auditable.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS saas_package_provision_steps (
    id TEXT PRIMARY KEY,
    purchase_id TEXT NOT NULL,
    step TEXT NOT NULL CHECK (char_length(step) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped', 'blocked')),
    outcome TEXT CHECK (outcome IS NULL OR char_length(outcome) BETWEEN 1 AND 64),
    detail TEXT,
    attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt >= 1),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_saas_package_provision_steps_purchase ON saas_package_provision_steps (purchase_id);
CREATE INDEX IF NOT EXISTS idx_saas_package_provision_steps_step ON saas_package_provision_steps (purchase_id, step);
CREATE INDEX IF NOT EXISTS idx_saas_package_provision_steps_status ON saas_package_provision_steps (status);

-- ---------------------------------------------------------------------------
-- Seed: the example "Online Exam SaaS" package (example data only;
-- an admin can edit or delete it like any other package). Ids match
-- the Rust seed rows in `src/modules/saas_packages.rs` exactly.
-- ---------------------------------------------------------------------------
INSERT INTO saas_packages (id, slug, name, description, status, current_version, product_id, preset_settings, wizard_steps, created_by, vendor_id)
VALUES (
    '20000000-0000-4000-8000-000000000001',
    'online-exam-saas',
    'Online Exam SaaS',
    'Everything a school or training provider needs to run online exams: exams, e-learning courses, academic records, notifications, and payments, provisioned on its own tenant subdomain with a guided setup wizard.',
    'published',
    1,
    NULL,
    '{"welcome_email_enabled": true, "default_language": "en"}'::jsonb,
    '["business_type", "review_modules", "apply", "settings", "done"]'::jsonb,
    'system',
    NULL
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO saas_package_modules (package_id, version, module_id, required, sort_order)
VALUES
    ('20000000-0000-4000-8000-000000000001', 1, 'exams', TRUE, 0),
    ('20000000-0000-4000-8000-000000000001', 1, 'elearning', TRUE, 1),
    ('20000000-0000-4000-8000-000000000001', 1, 'academic', TRUE, 2),
    ('20000000-0000-4000-8000-000000000001', 1, 'notifications', TRUE, 3),
    ('20000000-0000-4000-8000-000000000001', 1, 'payments', TRUE, 4)
ON CONFLICT (package_id, version, module_id) DO NOTHING;
