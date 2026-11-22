-- Storage Manager as a product + bring-your-own storage (BYO)
-- (`src/modules/storage_manager.rs`): storage plans (sellable tiers),
-- per-buyer plan assignments, usage metering reports, BYO connections
-- with rules and terms, terms versions and acceptances, BYO objects,
-- and BYO health checks.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- WARNING: this migration has never been executed against a live
-- PostgreSQL database. It was written from the module structs and
-- reviewed by reading it back; run it against a scratch database
-- and inspect the result before applying it anywhere real.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - Price is owned by Product Management / billing: `storage_plans`
--     stores only a `product_id` billing reference and a
--     `billing_cycle` hint. There is NO money column here, by design.
--   - `storage_plans.quota_bytes` is BIGINT bytes (GB is a display
--     unit only, computed by the API view). `max_files` 0 means
--     unlimited. `included_provider_kinds` holds canonical names
--     (`local`, `s3`, `r2`, `drive`, `sftp`, `s3-compatible`).
--   - `storage_plan_assignments` has one active row per buyer
--     (company and/or user): the unique expression index below
--     enforces one row per (company_id, user_id) pair, with an
--     empty company meaning a personal plan. `status` is `active`,
--     `cancelled`, or `expired`.
--   - `storage_usage_metering` is the billing handoff: one row per
--     (buyer, period `YYYY-MM`) report, matching the
--     `storage.metering.reported` event payload. Bytes and files
--     are BIGINT; no NUMERIC column is needed.
--   - `storage_byo_connections` stores a buyer's own external
--     storage. Credentials are write-only: only the ciphertext
--     from `encrypt_secret_value` (a versioned `v1.` HMAC keystream
--     with an HMAC tag) in `credentials_encrypted` and a short
--     fingerprint prefix in `key_fingerprint`. No response, log
--     line, hook payload, or usage record ever contains a
--     credential or a fragment of one. `provider_kind` holds the
--     `StorageKind` (`local`, `s3`, `r2`, `google_drive`, `sftp`);
--     `s3_compatible` records that the kind was connected as
--     `s3-compatible` (S3 kind with a custom endpoint, which is
--     required for it). `path_prefix` is never empty: the
--     connection may only touch keys under it. `status` is `draft`,
--     `validated`, `active`, `suspended`, or `revoked`.
--   - `storage_byo_connections.encryption_required` is a
--     requirement flag, not an enforcement proof: see the module
--     header for the honest per-provider matrix (S3/R2 send no
--     server-side-encryption header today; local is
--     filesystem-level only; Drive/SFTP are transport encryption
--     only).
--   - `storage_byo_terms` holds terms versions. Only one version
--     is current at a time (enforced by the API, not by a unique
--     index, because a current version may be retired without a
--     replacement). `reacceptance_deadline` is the instant after
--     which un-accepted connections are suspended by the
--     enforcement run.
--   - `storage_byo_terms_acceptances` records one acceptance per
--     (user, terms version). `connection_id` is set when the
--     acceptance was recorded for a specific connection.
--   - `storage_byo_objects` records objects stored through a BYO
--     connection (key relative to the connection prefix; the full
--     provider key is prefix + key). The connection counters
--     (`used_bytes` / `used_files`) are maintained from these rows.
--   - `storage_byo_health_checks` is an append-only audit of BYO
--     health runs (connectivity + write/read/delete probe).
--     `detail` never contains credentials or provider error bodies.
--   - There are deliberately NO foreign keys between these tables
--     (following migration 000033): assignments, acceptances, and
--     health rows must survive as audit rows even if the plan,
--     terms, or connection they reference is removed by an
--     operator at the SQL level.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One storage plan: a sellable tier.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    description VARCHAR NOT NULL DEFAULT '',
    quota_bytes BIGINT NOT NULL DEFAULT 0
        CHECK (quota_bytes >= 0),
    max_files BIGINT NOT NULL DEFAULT 0
        CHECK (max_files >= 0),
    included_provider_kinds TEXT[] NOT NULL DEFAULT '{}',
    product_id VARCHAR,
    billing_cycle VARCHAR
        CHECK (billing_cycle IN ('monthly', 'yearly', 'one_time', 'usage_based')),
    is_public BOOLEAN NOT NULL DEFAULT FALSE,
    sort_order BIGINT NOT NULL DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_plans_name
    ON storage_plans (name);
CREATE INDEX IF NOT EXISTS ix_storage_plans_is_public
    ON storage_plans (is_public);
CREATE INDEX IF NOT EXISTS ix_storage_plans_enabled
    ON storage_plans (enabled);
CREATE INDEX IF NOT EXISTS ix_storage_plans_product_id
    ON storage_plans (product_id);

-- ---------------------------------------------------------------------------
-- One buyer plan assignment (company and/or user).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_plan_assignments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID NOT NULL,
    company_id VARCHAR,
    user_id VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'cancelled', 'expired')),
    assigned_by VARCHAR NOT NULL DEFAULT '',
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    cancel_reason VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (expires_at IS NULL OR expires_at > assigned_at)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_plan_assignments_buyer
    ON storage_plan_assignments (user_id, COALESCE(company_id, ''));
CREATE INDEX IF NOT EXISTS ix_storage_plan_assignments_plan_id
    ON storage_plan_assignments (plan_id);
CREATE INDEX IF NOT EXISTS ix_storage_plan_assignments_company_id
    ON storage_plan_assignments (company_id);
CREATE INDEX IF NOT EXISTS ix_storage_plan_assignments_user_id
    ON storage_plan_assignments (user_id);
CREATE INDEX IF NOT EXISTS ix_storage_plan_assignments_status
    ON storage_plan_assignments (status);

-- ---------------------------------------------------------------------------
-- One metering report for billing (buyer + period).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_usage_metering (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR,
    user_id VARCHAR NOT NULL,
    plan_id UUID,
    period VARCHAR NOT NULL
        CHECK (period ~ '^[0-9]{4}-[0-9]{2}$'),
    bytes_stored BIGINT NOT NULL DEFAULT 0
        CHECK (bytes_stored >= 0),
    files_stored BIGINT NOT NULL DEFAULT 0
        CHECK (files_stored >= 0),
    reported_by VARCHAR NOT NULL DEFAULT '',
    reported_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_storage_usage_metering_company_id
    ON storage_usage_metering (company_id);
CREATE INDEX IF NOT EXISTS ix_storage_usage_metering_user_id
    ON storage_usage_metering (user_id);
CREATE INDEX IF NOT EXISTS ix_storage_usage_metering_plan_id
    ON storage_usage_metering (plan_id);
CREATE INDEX IF NOT EXISTS ix_storage_usage_metering_period
    ON storage_usage_metering (period);

-- ---------------------------------------------------------------------------
-- One bring-your-own storage connection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_byo_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    provider_kind VARCHAR NOT NULL
        CHECK (provider_kind IN ('local', 's3', 'r2', 'google_drive', 'sftp')),
    s3_compatible BOOLEAN NOT NULL DEFAULT FALSE,
    endpoint VARCHAR NOT NULL DEFAULT '',
    region VARCHAR NOT NULL DEFAULT '',
    container VARCHAR NOT NULL DEFAULT '',
    path VARCHAR NOT NULL DEFAULT '',
    sftp_host VARCHAR NOT NULL DEFAULT '',
    sftp_port INTEGER NOT NULL DEFAULT 0
        CHECK (sftp_port >= 0 AND sftp_port <= 65535),
    sftp_username VARCHAR NOT NULL DEFAULT '',
    sftp_auth_kind VARCHAR NOT NULL DEFAULT ''
        CHECK (sftp_auth_kind IN ('', 'keyfile', 'password')),
    sftp_keyfile VARCHAR NOT NULL DEFAULT '',
    allowed_hosts TEXT[] NOT NULL DEFAULT '{}',
    credentials_encrypted TEXT,
    key_fingerprint VARCHAR,
    path_prefix VARCHAR NOT NULL DEFAULT ''
        CHECK (path_prefix <> ''),
    owner_user VARCHAR NOT NULL,
    company_id VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'validated', 'active', 'suspended', 'revoked')),
    status_reason VARCHAR,
    allow_read BOOLEAN NOT NULL DEFAULT TRUE,
    allow_write BOOLEAN NOT NULL DEFAULT TRUE,
    allow_delete BOOLEAN NOT NULL DEFAULT FALSE,
    max_bytes BIGINT CHECK (max_bytes IS NULL OR max_bytes >= 0),
    max_files BIGINT CHECK (max_files IS NULL OR max_files >= 0),
    used_bytes BIGINT NOT NULL DEFAULT 0
        CHECK (used_bytes >= 0),
    used_files BIGINT NOT NULL DEFAULT 0
        CHECK (used_files >= 0),
    encryption_required BOOLEAN NOT NULL DEFAULT FALSE,
    allowed_extensions TEXT[] NOT NULL DEFAULT '{}',
    allowed_mime_types TEXT[] NOT NULL DEFAULT '{}',
    terms_version VARCHAR,
    terms_accepted_at TIMESTAMPTZ,
    last_validated_at TIMESTAMPTZ,
    last_validation_ok BOOLEAN,
    last_validation_error VARCHAR,
    last_health_at TIMESTAMPTZ,
    last_health_ok BOOLEAN,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_storage_byo_connections_owner_user
    ON storage_byo_connections (owner_user);
CREATE INDEX IF NOT EXISTS ix_storage_byo_connections_company_id
    ON storage_byo_connections (company_id);
CREATE INDEX IF NOT EXISTS ix_storage_byo_connections_status
    ON storage_byo_connections (status);
CREATE INDEX IF NOT EXISTS ix_storage_byo_connections_provider_kind
    ON storage_byo_connections (provider_kind);

-- ---------------------------------------------------------------------------
-- One version of the BYO terms of use.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_byo_terms (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    is_current BOOLEAN NOT NULL DEFAULT FALSE,
    reacceptance_deadline TIMESTAMPTZ,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by VARCHAR NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_byo_terms_version
    ON storage_byo_terms (version);
CREATE INDEX IF NOT EXISTS ix_storage_byo_terms_is_current
    ON storage_byo_terms (is_current);
CREATE INDEX IF NOT EXISTS ix_storage_byo_terms_enabled
    ON storage_byo_terms (enabled);

-- ---------------------------------------------------------------------------
-- One recorded acceptance of a BYO terms version by one user.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_byo_terms_acceptances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    terms_id UUID NOT NULL,
    terms_version VARCHAR NOT NULL,
    user_id VARCHAR NOT NULL,
    company_id VARCHAR,
    connection_id UUID,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_byo_terms_acceptances_user_version
    ON storage_byo_terms_acceptances (user_id, terms_version);
CREATE INDEX IF NOT EXISTS ix_storage_byo_terms_acceptances_terms_id
    ON storage_byo_terms_acceptances (terms_id);
CREATE INDEX IF NOT EXISTS ix_storage_byo_terms_acceptances_user_id
    ON storage_byo_terms_acceptances (user_id);
CREATE INDEX IF NOT EXISTS ix_storage_byo_terms_acceptances_connection_id
    ON storage_byo_terms_acceptances (connection_id);

-- ---------------------------------------------------------------------------
-- One object recorded through a BYO connection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_byo_objects (
    connection_id UUID NOT NULL,
    key VARCHAR NOT NULL,
    size_bytes BIGINT NOT NULL DEFAULT 0
        CHECK (size_bytes >= 0),
    mime VARCHAR NOT NULL DEFAULT '',
    owner_user VARCHAR NOT NULL,
    company_id VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (connection_id, key)
);

CREATE INDEX IF NOT EXISTS ix_storage_byo_objects_owner_user
    ON storage_byo_objects (owner_user);
CREATE INDEX IF NOT EXISTS ix_storage_byo_objects_company_id
    ON storage_byo_objects (company_id);

-- ---------------------------------------------------------------------------
-- BYO health-check runs (append-only audit).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_byo_health_checks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id UUID NOT NULL,
    ok BOOLEAN NOT NULL,
    latency_ms BIGINT NOT NULL DEFAULT 0 CHECK (latency_ms >= 0),
    detail VARCHAR NOT NULL DEFAULT '',
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_storage_byo_health_checks_connection_id
    ON storage_byo_health_checks (connection_id);
CREATE INDEX IF NOT EXISTS ix_storage_byo_health_checks_checked_at
    ON storage_byo_health_checks (checked_at);
CREATE INDEX IF NOT EXISTS ix_storage_byo_health_checks_ok
    ON storage_byo_health_checks (ok);
