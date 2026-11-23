-- SSL certificate modes + Cloudflare detection tables for the
-- Unified Pentagonal Project (certificates.rs, cert_swap.rs).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `domain_cert_modes` holds one row per mode change; the current
--     mode for a domain is the newest row for that domain
--     (created_at order). `mode` is a registry id (seeded
--     `letsencrypt`, `custom`, `cloudflare`, or an admin-defined id
--     from `PUT /cert-modes/definitions`): no CHECK constraint on
--     `mode`, because the registry is admin-extensible by design.
--   - `cloudflare_detections` holds one stored detection result per
--     domain (`UNIQUE(domain)`); `methods` is the per-method result
--     object (`dns`, `probe`, `api`) as JSONB.
--   - `cloudflare_ip_ranges` is a singleton row keyed by `key =
--     'global'`, the same pattern as `certificate_settings`: the
--     ranges are fetched data with a fetch date (`fetched_at`), never
--     code constants. Refresh through
--     `POST /api/v1/certificates/cloudflare-ranges/refresh`.
--   - Private keys and PEM material are never stored in these
--     tables: the TLS storage owns them (see `crate::http::tls`).
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One certificate-mode change per row: the newest row per domain is
-- the domain's current mode; earlier rows are its mode history.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS domain_cert_modes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain VARCHAR NOT NULL,
    mode VARCHAR NOT NULL,
    provider VARCHAR NOT NULL,
    fingerprint_sha256 VARCHAR NOT NULL DEFAULT '',
    installed_at TIMESTAMPTZ,
    previous_fingerprint VARCHAR,
    status VARCHAR NOT NULL CHECK (status IN ('active', 'pending', 'awaiting_upload', 'error')),
    last_error TEXT,
    updated_by VARCHAR NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_domain_cert_modes_domain
    ON domain_cert_modes (domain);
CREATE INDEX IF NOT EXISTS idx_domain_cert_modes_domain_created
    ON domain_cert_modes (domain, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_domain_cert_modes_mode
    ON domain_cert_modes (mode);
CREATE INDEX IF NOT EXISTS idx_domain_cert_modes_status
    ON domain_cert_modes (status);

-- ---------------------------------------------------------------------------
-- One stored Cloudflare detection result per domain.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cloudflare_detections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain VARCHAR NOT NULL,
    detected BOOLEAN NOT NULL DEFAULT FALSE,
    methods JSONB NOT NULL DEFAULT '{}'::jsonb,
    ssl_mode VARCHAR,
    end_to_end_encrypted BOOLEAN,
    flexible_warning TEXT,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (domain)
);

CREATE INDEX IF NOT EXISTS idx_cloudflare_detections_domain
    ON cloudflare_detections (domain);
CREATE INDEX IF NOT EXISTS idx_cloudflare_detections_detected
    ON cloudflare_detections (detected);
CREATE INDEX IF NOT EXISTS idx_cloudflare_detections_checked_at
    ON cloudflare_detections (checked_at);

-- ---------------------------------------------------------------------------
-- Stored Cloudflare IP-range snapshot (singleton row): fetched ranges
-- plus their fetch date and sources. Empty lists mean "never fetched
-- successfully"; the DNS detection method reports that honestly.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cloudflare_ip_ranges (
    key VARCHAR PRIMARY KEY DEFAULT 'global',
    v4 JSONB NOT NULL DEFAULT '[]'::jsonb,
    v6 JSONB NOT NULL DEFAULT '[]'::jsonb,
    fetched_at TIMESTAMPTZ,
    source_v4 VARCHAR NOT NULL DEFAULT 'https://www.cloudflare.com/ips-v4',
    source_v6 VARCHAR NOT NULL DEFAULT 'https://www.cloudflare.com/ips-v6',
    fetch_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
