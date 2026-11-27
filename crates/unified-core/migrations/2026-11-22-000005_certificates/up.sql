-- SSL / TLS certificate tables for the Unified Pentagonal Project
-- (certificates.rs): certificates known to the module, the renewal
-- history, and the module settings.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `certificates.provider` is the ACME provider key
--     (`letsencrypt`, `google`, `zerossl`, `buypass`, `custom`) or
--     `self-signed` for fallback certificates.
--   - `certificate_settings` is a singleton row keyed by `key =
--     'global'`, the same pattern as `vcs_provider_settings`.
--     The ACME configuration (directory URL, staging flag, contacts,
--     EAB credentials, key type / size / curve, challenge kind,
--     renewal thresholds) is stored as one JSONB document because
--     the code keeps it as a single `AcmeConfig` value; EAB secrets
--     inside it must be encrypted before insert, the same reservation
--     as the `*_encrypted` columns of `vcs_provider_settings` (no
--     encryption helper exists yet).
--   - Private keys and PEM material are never stored in these
--     tables: the TLS storage owns them (see `crate::http::tls`).
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One certificate known to the module: configured, renewed,
-- installed, or observed as a fallback.
CREATE TABLE IF NOT EXISTS certificates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain VARCHAR NOT NULL,
    provider VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('valid', 'expiring', 'expired', 'invalid', 'self-signed', 'revoked')),
    issuer VARCHAR NOT NULL DEFAULT '',
    sans JSONB NOT NULL DEFAULT '[]'::jsonb,
    fingerprint_sha256 VARCHAR NOT NULL,
    not_before TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    days_remaining BIGINT,
    self_signed BOOLEAN NOT NULL DEFAULT FALSE,
    wildcard BOOLEAN NOT NULL DEFAULT FALSE,
    source VARCHAR NOT NULL CHECK (source IN ('configured', 'dynamic', 'fallback', 'custom')),
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    last_renewed_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_certificates_domain
    ON certificates (domain);
CREATE INDEX IF NOT EXISTS idx_certificates_status
    ON certificates (status);
CREATE INDEX IF NOT EXISTS idx_certificates_expires_at
    ON certificates (expires_at);
CREATE INDEX IF NOT EXISTS idx_certificates_fingerprint_sha256
    ON certificates (fingerprint_sha256);

-- One renewal attempt, for the renewal history log.
CREATE TABLE IF NOT EXISTS certificate_renewals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain VARCHAR NOT NULL,
    provider VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('started', 'succeeded', 'failed', 'skipped')),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    error TEXT,
    new_expires_at TIMESTAMPTZ,
    forced BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_certificate_renewals_domain
    ON certificate_renewals (domain);
CREATE INDEX IF NOT EXISTS idx_certificate_renewals_status
    ON certificate_renewals (status);

-- Module settings (singleton row): whether ACME is enabled, how
-- often the expiry check runs, and the ACME configuration document.
CREATE TABLE IF NOT EXISTS certificate_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    check_interval_secs BIGINT NOT NULL DEFAULT 86400 CHECK (check_interval_secs > 0),
    acme JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
