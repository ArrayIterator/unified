-- Core tables for the Unified Pentagonal Project.
-- Matches src/db/schema.rs exactly. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Roles: static default roles plus dynamically created roles.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS roles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    slug VARCHAR NOT NULL UNIQUE,
    description TEXT,
    is_system BOOLEAN NOT NULL DEFAULT FALSE,
    permissions JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Companies: multi-company / multi-site basis (immutable site basis).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS companies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    slug VARCHAR NOT NULL UNIQUE,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Users.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR NOT NULL UNIQUE,
    name VARCHAR NOT NULL,
    password_hash VARCHAR NOT NULL,
    totp_secret VARCHAR,
    totp_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_users_company_id ON users (company_id);
CREATE INDEX IF NOT EXISTS idx_users_is_active ON users (is_active);

-- ---------------------------------------------------------------------------
-- User / role assignments (many-to-many).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS user_roles (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role_id UUID NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, role_id)
);

CREATE INDEX IF NOT EXISTS idx_user_roles_role_id ON user_roles (role_id);

-- ---------------------------------------------------------------------------
-- Company sites: each company may own several sites.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS company_sites (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    name VARCHAR NOT NULL,
    domain VARCHAR NOT NULL UNIQUE,
    is_primary BOOLEAN NOT NULL DEFAULT FALSE,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_company_sites_company_id ON company_sites (company_id);

-- ---------------------------------------------------------------------------
-- Domains, linked to a company and optionally to one of its sites.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS domains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL UNIQUE,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    site_id UUID REFERENCES company_sites (id) ON DELETE CASCADE,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    ssl_status VARCHAR NOT NULL DEFAULT 'none',
    ssl_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_domains_company_id ON domains (company_id);
CREATE INDEX IF NOT EXISTS idx_domains_site_id ON domains (site_id);
CREATE INDEX IF NOT EXISTS idx_domains_ssl_status ON domains (ssl_status);

-- ---------------------------------------------------------------------------
-- Geo overrides: settings-defined IP range -> country mappings that win
-- over the GeoLite database reading.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS geo_overrides (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cidr VARCHAR NOT NULL UNIQUE,
    country_code VARCHAR NOT NULL,
    country_name VARCHAR,
    city VARCHAR,
    asn INTEGER,
    asn_organization VARCHAR,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_geo_overrides_is_active ON geo_overrides (is_active);
CREATE INDEX IF NOT EXISTS idx_geo_overrides_country_code ON geo_overrides (country_code);

-- ---------------------------------------------------------------------------
-- Licenses and their activations.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS licenses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    license_key VARCHAR NOT NULL UNIQUE,
    product VARCHAR NOT NULL,
    user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL,
    max_activations INTEGER NOT NULL DEFAULT 1,
    status VARCHAR NOT NULL DEFAULT 'active',
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_licenses_user_id ON licenses (user_id);
CREATE INDEX IF NOT EXISTS idx_licenses_company_id ON licenses (company_id);
CREATE INDEX IF NOT EXISTS idx_licenses_status ON licenses (status);
CREATE INDEX IF NOT EXISTS idx_licenses_product ON licenses (product);

CREATE TABLE IF NOT EXISTS license_activations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    license_id UUID NOT NULL REFERENCES licenses (id) ON DELETE CASCADE,
    instance_id VARCHAR NOT NULL,
    hostname VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (license_id, instance_id)
);

CREATE INDEX IF NOT EXISTS idx_license_activations_license_id ON license_activations (license_id);

-- ---------------------------------------------------------------------------
-- Seed the static default roles. Idempotent: re-running the migration
-- keeps existing roles untouched.
-- ---------------------------------------------------------------------------
INSERT INTO roles (name, slug, description, is_system, permissions) VALUES
    ('Administrator', 'admin', 'Full access to every part of the system.', TRUE,
     '{"users.manage": true, "users.view": true, "roles.manage": true, "companies.manage": true, "domains.manage": true, "licenses.manage": true, "content.create": true, "content.edit": true, "content.publish": true, "content.delete": true, "media.manage": true, "settings.manage": true}'::jsonb),
    ('Editor', 'editor', 'Can create, edit, publish and delete content.', TRUE,
     '{"content.create": true, "content.edit": true, "content.publish": true, "content.delete": true, "media.manage": true, "licenses.view": true, "users.view": true}'::jsonb),
    ('Author', 'author', 'Can create and edit their own content.', TRUE,
     '{"content.create": true, "content.edit": true, "media.manage": true, "profile.view": true}'::jsonb),
    ('Member', 'member', 'Standard authenticated user.', TRUE,
     '{"content.view": true, "profile.view": true}'::jsonb),
    ('Customer', 'customer', 'Customer with access to their own licenses and orders.', TRUE,
     '{"licenses.view": true, "licenses.activate": true, "orders.view": true, "profile.view": true}'::jsonb)
ON CONFLICT (slug) DO NOTHING;
