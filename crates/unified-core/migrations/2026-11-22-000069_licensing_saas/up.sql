-- Licensing as a service for the Unified Pentagonal Project
-- (`src/modules/licensing.rs`, batch 7): per-product licensing
-- settings, git distribution channels, versioned releases, a
-- hash-chained license event log, signed offline-token records, and
-- activation history. Matches src/db/schema.rs conventions.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `license_products.product` is the slug referenced by
--     `licenses.product`, `license_distributions.product`,
--     `license_releases.product`, and `license_offline_tokens.product`.
--     It is TEXT, not a foreign key, because `licenses.product` is
--     itself free-form TEXT (a purchased item name); the slug is
--     unique here so the integrator can join on it.
--   - `license_distributions.webhook_secret_hash` stores ONLY the
--     SHA-256 hex of the webhook secret. The raw secret is held in
--     memory only (it is needed to verify the provider HMAC) and is
--     never persisted; after a restart a distribution must be
--     re-configured unless the integrator adds encrypted persistence.
--   - `license_offline_tokens` stores NO token value, only the
--     token id and expiry: a token is
--     base64url(payload).base64url(HMAC-SHA256(payload)) and is
--     verified from its signature, not from this table. `revoked`
--     only affects online checks; already-issued offline
--     verifications are unaffected by design.
--   - `license_events.hash` is the SHA-256 hex of the canonical
--     payload `prev_hash || '\n' || license_id || '\n' || event ||
--     '\n' || actor || '\n' || details_json`; `prev_hash` is the
--     previous entry's hash, or 'GENESIS' for the first entry of a
--     license. The chain is verified in memory by
--     `verify_event_chain` in the module.
--   - `license_activation_history` and `license_events` are audit
--     records: deleting a license does not delete them.
--   - Money: none here. All ids are Uuid, all timestamps are
--     Timestamptz.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Per-product licensing settings: key format, seat defaults, validity,
-- grace period, renewal reminders, offline-token allowance, and the
-- allowed distribution channels.
CREATE TABLE IF NOT EXISTS license_products (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product TEXT NOT NULL UNIQUE CHECK (product ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
    display_name TEXT,
    owner_id UUID REFERENCES users (id) ON DELETE SET NULL,
    key_prefix TEXT CHECK (key_prefix IS NULL OR key_prefix ~ '^[A-Z0-9]{1,8}$'),
    segments INTEGER NOT NULL DEFAULT 4 CHECK (segments BETWEEN 1 AND 8),
    key_format TEXT NOT NULL DEFAULT 'standard' CHECK (key_format IN ('standard', 'custom')),
    key_pattern TEXT,
    default_max_activations INTEGER NOT NULL DEFAULT 1 CHECK (default_max_activations >= 1),
    default_validity_days BIGINT,
    grace_period_days BIGINT NOT NULL DEFAULT 0 CHECK (grace_period_days BETWEEN 0 AND 365),
    renewal_reminder_days BIGINT[] NOT NULL DEFAULT ARRAY[30, 14, 7, 1],
    offline_max_days BIGINT NOT NULL DEFAULT 30 CHECK (offline_max_days BETWEEN 1 AND 90),
    allowed_distribution_channels TEXT[] NOT NULL DEFAULT ARRAY['git', 'api', 'file'],
    api_allowed_endpoints TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    api_rate_limit_per_minute BIGINT NOT NULL DEFAULT 60 CHECK (api_rate_limit_per_minute >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (key_format <> 'custom' OR key_pattern IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_license_products_owner_id ON license_products (owner_id);

-- One git distribution channel for a product: provider, repository,
-- branch/tag reference, and the SHA-256 hash of the webhook secret
-- (the raw secret is never stored).
CREATE TABLE IF NOT EXISTS license_distributions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('github', 'gitlab', 'bitbucket', 'generic')),
    repo TEXT NOT NULL CHECK (repo ~ '^[A-Za-z0-9._-]+/[A-Za-z0-9._/-]+$'),
    git_ref TEXT NOT NULL DEFAULT 'main',
    webhook_secret_hash TEXT CHECK (webhook_secret_hash IS NULL OR webhook_secret_hash ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product, provider)
);

CREATE INDEX IF NOT EXISTS idx_license_distributions_product ON license_distributions (product);

-- One uploaded build/package attached to a licensed product.
-- `storage_key` is the object key in `storage_target`, or the
-- in-memory blob key when no storage target was registered.
CREATE TABLE IF NOT EXISTS license_releases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES license_products (id) ON DELETE CASCADE,
    product TEXT NOT NULL,
    version TEXT NOT NULL CHECK (version ~ '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'),
    file_name TEXT NOT NULL,
    storage_target TEXT,
    storage_key TEXT NOT NULL,
    sha256_hex TEXT NOT NULL CHECK (sha256_hex ~ '^[0-9a-f]{64}$'),
    size_bytes BIGINT NOT NULL CHECK (size_bytes > 0),
    download_count BIGINT NOT NULL DEFAULT 0 CHECK (download_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, version)
);

CREATE INDEX IF NOT EXISTS idx_license_releases_product ON license_releases (product);
CREATE INDEX IF NOT EXISTS idx_license_releases_product_id ON license_releases (product_id);

-- Hash-chained license event log. `prev_hash` is the previous
-- entry's hash for the same license, or 'GENESIS' for the first
-- entry; `hash` is SHA-256 over the canonical payload (see the
-- header notes). Events are append-only by convention: no update
-- or delete path exists in the module.
CREATE TABLE IF NOT EXISTS license_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    license_id UUID NOT NULL REFERENCES licenses (id) ON DELETE CASCADE,
    event TEXT NOT NULL,
    actor TEXT,
    details JSONB NOT NULL DEFAULT '{}'::JSONB,
    prev_hash TEXT NOT NULL,
    hash TEXT NOT NULL CHECK (hash ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_license_events_license_id ON license_events (license_id, created_at);
CREATE INDEX IF NOT EXISTS idx_license_events_event ON license_events (event);

-- Signed offline-token records (token value is never stored).
-- `revoked` only affects online checks.
CREATE TABLE IF NOT EXISTS license_offline_tokens (
    token_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    license_id UUID NOT NULL REFERENCES licenses (id) ON DELETE CASCADE,
    product TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_license_offline_tokens_license_id ON license_offline_tokens (license_id);
CREATE INDEX IF NOT EXISTS idx_license_offline_tokens_expires_at ON license_offline_tokens (expires_at);

-- Activation history: every activate and deactivate, with actor,
-- instance, hostname, and timestamp. Audit records: kept even when
-- the license is deleted.
CREATE TABLE IF NOT EXISTS license_activation_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    license_id UUID NOT NULL REFERENCES licenses (id) ON DELETE CASCADE,
    action TEXT NOT NULL CHECK (action IN ('activated', 'deactivated')),
    actor TEXT,
    instance_id TEXT NOT NULL,
    hostname TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_license_activation_history_license_id ON license_activation_history (license_id, created_at);
