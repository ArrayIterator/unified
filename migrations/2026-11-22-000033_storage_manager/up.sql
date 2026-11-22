-- Storage Manager module tables for the Unified Pentagonal Project
-- (`src/modules/storage_manager.rs`): bucket registry, per-module
-- bindings, quotas, daily usage rollups, lifecycle rules, signed-URL
-- revocations, and health checks.
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
--   - Credentials are write-only: `storage_buckets` stores only the
--     ciphertext from `encrypt_secret_value` (a versioned `v1.`
--     HMAC keystream with an HMAC tag) in `credentials_encrypted`
--     and a short fingerprint prefix in `key_fingerprint`. No
--     response, log line, hook payload, or usage record ever
--     contains a credential or a fragment of one. Both columns are
--     NULL when no credentials are set (a local bucket needs none).
--   - `storage_buckets.provider_kind` is `local`, `s3`, `r2`,
--     `google_drive`, or `sftp` (`StorageKind`). `sftp_*` columns
--     hold the SFTP host / port / username / auth kind / keyfile
--     path / base path; the SFTP password lives inside the encrypted
--     credential JSON, never in its own column. `path` is the local
--     base directory for `local` buckets and the remote base path
--     for `sftp` buckets.
--   - `storage_buckets.encryption_at_rest` is a requirement flag,
--     not an enforcement proof: see the module header for the honest
--     per-provider matrix (S3/R2 send no server-side-encryption
--     header today; local is filesystem-level only; Drive/SFTP are
--     transport encryption only).
--   - `storage_bindings` has one enabled row per (module_id,
--     company_id): the unique partial index below enforces it.
--     `company_id` NULL means the binding applies to every company.
--     There is deliberately NO foreign key from `bucket_id` to
--     `storage_buckets`: the module refuses to delete a bucket with
--     enabled bindings at the API layer, and a binding must survive
--     as an audit row even if its bucket is removed by an operator
--     at the SQL level.
--   - `storage_quotas` has one row per (scope, scope_id), enforced
--     by a unique index. `scope` is `company`, `user`, or `module`.
--     Limits are BIGINT bytes / object counts, never money: the
--     NUMERIC(19,4) rule does not apply. NULL means unlimited.
--     `used_bytes` / `used_objects` are maintained by the module on
--     every recorded upload / delete.
--   - `storage_usage_daily` is the daily rollup: one row per
--     (date, bucket_id, company_id, user_id, module_id). `bytes_total`
--     / `objects_total` are the running totals for that day;
--     `bytes_delta` / `objects_delta` are the net change recorded
--     that day (uploads add, deletes subtract from the totals).
--   - `storage_lifecycle_rules.retention_days` is 0 or more days;
--     `auto_delete` and `archive_transition` are never both false
--     (the module enforces it; the CHECK below enforces it too).
--   - `storage_signed_url_revocations` records issued hub download
--     tokens for audit and revocation. The token VALUE is never
--     stored here: only the record id and metadata. A token is
--     revoked by setting `revoked_at`; `expires_at` is enforced on
--     every redemption. Provider (presigned) URLs issued alongside
--     a hub token cannot be revoked by the hub — see
--     `provider_url_issued` and the module header.
--   - `storage_health_checks` is an append-only audit of health
--     runs (connectivity + write/read/delete probe). `detail` never
--     contains credentials or provider error bodies.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One registered storage bucket (container).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_buckets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    provider_kind VARCHAR NOT NULL
        CHECK (provider_kind IN ('local', 's3', 'r2', 'google_drive', 'sftp')),
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
    encryption_at_rest BOOLEAN NOT NULL DEFAULT FALSE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    company_id VARCHAR,
    created_by VARCHAR NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_buckets_name
    ON storage_buckets (name);
CREATE INDEX IF NOT EXISTS ix_storage_buckets_provider_kind
    ON storage_buckets (provider_kind);
CREATE INDEX IF NOT EXISTS ix_storage_buckets_company_id
    ON storage_buckets (company_id);
CREATE INDEX IF NOT EXISTS ix_storage_buckets_enabled
    ON storage_buckets (enabled);

-- ---------------------------------------------------------------------------
-- One per-module binding: which module stores where.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_bindings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    module_id VARCHAR NOT NULL,
    bucket_id UUID NOT NULL,
    key_prefix VARCHAR NOT NULL DEFAULT '',
    company_id VARCHAR,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_storage_bindings_module_id
    ON storage_bindings (module_id);
CREATE INDEX IF NOT EXISTS ix_storage_bindings_bucket_id
    ON storage_bindings (bucket_id);
CREATE INDEX IF NOT EXISTS ix_storage_bindings_company_id
    ON storage_bindings (company_id);
-- One enabled binding per (module_id, company_id). Disabled rows are
-- kept as history, so the uniqueness is a partial index, not a
-- table constraint.
CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_bindings_enabled_module_company
    ON storage_bindings (module_id, COALESCE(company_id, ''))
    WHERE enabled;

-- ---------------------------------------------------------------------------
-- One quota: soft (warn) and hard (reject) limits for one scope.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope VARCHAR NOT NULL
        CHECK (scope IN ('company', 'user', 'module')),
    scope_id VARCHAR NOT NULL,
    bytes_limit BIGINT CHECK (bytes_limit IS NULL OR bytes_limit >= 0),
    objects_limit BIGINT CHECK (objects_limit IS NULL OR objects_limit >= 0),
    soft_bytes_limit BIGINT CHECK (soft_bytes_limit IS NULL OR soft_bytes_limit >= 0),
    soft_objects_limit BIGINT CHECK (soft_objects_limit IS NULL OR soft_objects_limit >= 0),
    used_bytes BIGINT NOT NULL DEFAULT 0 CHECK (used_bytes >= 0),
    used_objects BIGINT NOT NULL DEFAULT 0 CHECK (used_objects >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (soft_bytes_limit IS NULL OR bytes_limit IS NULL OR soft_bytes_limit <= bytes_limit),
    CHECK (soft_objects_limit IS NULL OR objects_limit IS NULL OR soft_objects_limit <= objects_limit)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_quotas_scope_scope_id
    ON storage_quotas (scope, scope_id);
CREATE INDEX IF NOT EXISTS ix_storage_quotas_scope
    ON storage_quotas (scope);

-- ---------------------------------------------------------------------------
-- Daily usage rollup: one row per (date, bucket, company, user, module).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_usage_daily (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    date DATE NOT NULL,
    bucket_id UUID NOT NULL,
    company_id VARCHAR,
    user_id VARCHAR NOT NULL,
    module_id VARCHAR NOT NULL,
    bytes_total BIGINT NOT NULL DEFAULT 0 CHECK (bytes_total >= 0),
    objects_total BIGINT NOT NULL DEFAULT 0 CHECK (objects_total >= 0),
    bytes_delta BIGINT NOT NULL DEFAULT 0,
    objects_delta BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_storage_usage_daily_key
    ON storage_usage_daily (date, bucket_id, COALESCE(company_id, ''), user_id, module_id);
CREATE INDEX IF NOT EXISTS ix_storage_usage_daily_date
    ON storage_usage_daily (date);
CREATE INDEX IF NOT EXISTS ix_storage_usage_daily_bucket_id
    ON storage_usage_daily (bucket_id);
CREATE INDEX IF NOT EXISTS ix_storage_usage_daily_module_id
    ON storage_usage_daily (module_id);
CREATE INDEX IF NOT EXISTS ix_storage_usage_daily_user_id
    ON storage_usage_daily (user_id);
CREATE INDEX IF NOT EXISTS ix_storage_usage_daily_company_id
    ON storage_usage_daily (company_id);

-- ---------------------------------------------------------------------------
-- One lifecycle rule for one bucket.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_lifecycle_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bucket_id UUID NOT NULL,
    retention_days BIGINT NOT NULL DEFAULT 30 CHECK (retention_days >= 0),
    auto_delete BOOLEAN NOT NULL DEFAULT TRUE,
    archive_transition BOOLEAN NOT NULL DEFAULT FALSE,
    archive_prefix VARCHAR NOT NULL DEFAULT 'archive',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_run_at TIMESTAMPTZ,
    last_run_summary TEXT,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (auto_delete OR archive_transition)
);

CREATE INDEX IF NOT EXISTS ix_storage_lifecycle_rules_bucket_id
    ON storage_lifecycle_rules (bucket_id);
CREATE INDEX IF NOT EXISTS ix_storage_lifecycle_rules_enabled
    ON storage_lifecycle_rules (enabled);

-- ---------------------------------------------------------------------------
-- Issued hub download tokens (audit + revocation). The token value is
-- never stored: only metadata. Revocation sets `revoked_at`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_signed_url_revocations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bucket_id UUID NOT NULL,
    key VARCHAR NOT NULL,
    issued_by VARCHAR NOT NULL DEFAULT '',
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    revoke_reason VARCHAR,
    provider_url_issued BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (expires_at > created_at)
);

CREATE INDEX IF NOT EXISTS ix_storage_signed_url_revocations_bucket_id
    ON storage_signed_url_revocations (bucket_id);
CREATE INDEX IF NOT EXISTS ix_storage_signed_url_revocations_expires_at
    ON storage_signed_url_revocations (expires_at);
CREATE INDEX IF NOT EXISTS ix_storage_signed_url_revocations_revoked_at
    ON storage_signed_url_revocations (revoked_at);
CREATE INDEX IF NOT EXISTS ix_storage_signed_url_revocations_issued_by
    ON storage_signed_url_revocations (issued_by);

-- ---------------------------------------------------------------------------
-- Health-check runs (append-only audit).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS storage_health_checks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bucket_id UUID NOT NULL,
    ok BOOLEAN NOT NULL,
    latency_ms BIGINT NOT NULL DEFAULT 0 CHECK (latency_ms >= 0),
    detail VARCHAR NOT NULL DEFAULT '',
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_storage_health_checks_bucket_id
    ON storage_health_checks (bucket_id);
CREATE INDEX IF NOT EXISTS ix_storage_health_checks_checked_at
    ON storage_health_checks (checked_at);
CREATE INDEX IF NOT EXISTS ix_storage_health_checks_ok
    ON storage_health_checks (ok);
