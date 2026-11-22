-- Privacy module tables for the Unified Pentagonal Project
-- (`src/modules/privacy.rs`): declared personal-data fields per
-- module, one-way anonymization records, per-module visibility
-- defaults, consent types / records / history, right-to-erasure
-- requests, versioned privacy notices, and the privacy audit
-- trail. Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` columns are TEXT with NO foreign key on purpose:
--     actor ids in this codebase are opaque strings (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table. The users module stays the source of
--     truth for identity; privacy records here only reference users
--     by id and never duplicate user rows.
--   - `company_id` does not appear in this migration: privacy
--     records are scoped to users and modules, not companies.
--   - `privacy_anonymizations.token` and `field_tokens` hold
--     HMAC-SHA-256 hex values only. The anonymization key is
--     write-only server configuration (environment variable
--     `UNIFIED_PRIVACY_ANONYMIZATION_KEY`) and is never stored
--     in this schema. No original personal data is stored in
--     any table here: tokens are derived and not reversible.
--   - `privacy_anonymizations.user_id` is UNIQUE: one record per
--     user. Re-anonymizing the same user yields the same tokens
--     and only bumps `times_applied`.
--   - `privacy_consents` is one row per (user, consent type)
--     (`UNIQUE (user_id, consent_type)`); every grant or
--     withdrawal appends a row to `privacy_consent_history`, so
--     the current row is never the only record of a change.
--   - `privacy_notices` is versioned per module
--     (`UNIQUE (module_id, version)`); the current notice is the
--     highest version whose `effective_from` has passed. A module
--     with no notice row falls back to the generic notice text
--     in the module code, which is never stored here.
--   - `privacy_erasure_requests.anonymization_id` references
--     `privacy_anonymizations (id)` ON DELETE SET NULL so that
--     deleting an anonymization record never deletes the erasure
--     request that recorded it. `privacy_consent_history`
--     references `privacy_consents (id)` ON DELETE CASCADE.
--   - `privacy_visibility_defaults.module_id` is UNIQUE: one
--     default per module. Absence of a row means private, the
--     narrowest visibility, enforced by the module code.
--   - `privacy_field_declarations` is one row per
--     (module, field) (`UNIQUE (module_id, field_name)`). A
--     module with no declaration rows allows no personal-data
--     fields at all.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One declared personal-data field for one module: the field may be
-- collected, and only for the stated purpose. Created before any
-- other table because nothing references it; it is the allowlist
-- that payload validation checks against.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_field_declarations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    module_id TEXT NOT NULL,
    field_name TEXT NOT NULL,
    purpose TEXT NOT NULL,
    required BOOLEAN NOT NULL DEFAULT FALSE,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (module_id, field_name)
);

CREATE INDEX IF NOT EXISTS idx_privacy_field_declarations_module_id
    ON privacy_field_declarations (module_id);
CREATE INDEX IF NOT EXISTS idx_privacy_field_declarations_field_name
    ON privacy_field_declarations (field_name);

-- ---------------------------------------------------------------------------
-- One anonymization record per user: derived HMAC-SHA-256 tokens
-- only, never an original value. Created before
-- `privacy_erasure_requests` because erasure fulfillment
-- references it (`anonymization_id`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_anonymizations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    token TEXT NOT NULL CHECK (token ~ '^[0-9a-f]{64}$'),
    field_tokens JSONB NOT NULL DEFAULT '{}',
    scope_modules TEXT[] NOT NULL DEFAULT '{}',
    reason TEXT,
    actor TEXT NOT NULL,
    times_applied BIGINT NOT NULL DEFAULT 1 CHECK (times_applied >= 1),
    anonymized_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id)
);

CREATE INDEX IF NOT EXISTS idx_privacy_anonymizations_user_id
    ON privacy_anonymizations (user_id);
CREATE INDEX IF NOT EXISTS idx_privacy_anonymizations_token
    ON privacy_anonymizations (token);

-- ---------------------------------------------------------------------------
-- One visibility default per module. Absence of a row means
-- private. A row wider than private must have been set explicitly
-- (`explicit = TRUE`); the module code refuses widening without
-- an explicit confirmation.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_visibility_defaults (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    module_id TEXT NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'private'
        CHECK (visibility IN ('private', 'company', 'public')),
    explicit BOOLEAN NOT NULL DEFAULT FALSE,
    configured_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (module_id)
);

CREATE INDEX IF NOT EXISTS idx_privacy_visibility_defaults_module_id
    ON privacy_visibility_defaults (module_id);

-- ---------------------------------------------------------------------------
-- One consent record per (user, consent type). `granted` is the
-- latest decision; `withdrawn_at` and `expires_at` derive the
-- current status (`granted` / `withdrawn` / `expired`) in the
-- module code. Created before `privacy_consent_history` because
-- history rows reference it.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_consents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    consent_type TEXT NOT NULL,
    granted BOOLEAN NOT NULL DEFAULT TRUE,
    granted_at TIMESTAMPTZ,
    withdrawn_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    source TEXT NOT NULL DEFAULT 'api',
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, consent_type),
    CHECK (expires_at IS NULL OR granted_at IS NULL OR expires_at > granted_at)
);

CREATE INDEX IF NOT EXISTS idx_privacy_consents_user_id
    ON privacy_consents (user_id);
CREATE INDEX IF NOT EXISTS idx_privacy_consents_consent_type
    ON privacy_consents (consent_type);
CREATE INDEX IF NOT EXISTS idx_privacy_consents_user_id_consent_type
    ON privacy_consents (user_id, consent_type);

-- ---------------------------------------------------------------------------
-- One history entry per consent grant or withdrawal. Append-only:
-- the module never updates or deletes these rows.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_consent_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    consent_id UUID NOT NULL REFERENCES privacy_consents (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id TEXT NOT NULL,
    consent_type TEXT NOT NULL,
    action TEXT NOT NULL
        CHECK (action IN ('granted', 'withdrawn')),
    actor TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_privacy_consent_history_consent_id
    ON privacy_consent_history (consent_id);
CREATE INDEX IF NOT EXISTS idx_privacy_consent_history_user_id
    ON privacy_consent_history (user_id);

-- ---------------------------------------------------------------------------
-- One right-to-erasure request: scope (user id plus affected
-- modules), review state, and the anonymization record that
-- fulfilled it, if any. Fulfillment drives anonymization in the
-- module code; the token stored here is only a reference to the
-- anonymization row, never personal data itself.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_erasure_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    modules TEXT[] NOT NULL DEFAULT '{}',
    reason TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'fulfilled', 'rejected')),
    requested_by TEXT NOT NULL,
    reviewed_by TEXT,
    reviewed_at TIMESTAMPTZ,
    rejection_reason TEXT,
    fulfilled_at TIMESTAMPTZ,
    anonymization_id UUID REFERENCES privacy_anonymizations (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (status <> 'fulfilled' OR fulfilled_at IS NOT NULL),
    CHECK (status <> 'rejected' OR rejection_reason IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_privacy_erasure_requests_user_id
    ON privacy_erasure_requests (user_id);
CREATE INDEX IF NOT EXISTS idx_privacy_erasure_requests_status
    ON privacy_erasure_requests (status);
CREATE INDEX IF NOT EXISTS idx_privacy_erasure_requests_anonymization_id
    ON privacy_erasure_requests (anonymization_id);

-- ---------------------------------------------------------------------------
-- One versioned privacy notice for one module. The current notice
-- is the highest version whose `effective_from` has passed.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_notices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    module_id TEXT NOT NULL,
    text TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version >= 1),
    effective_from TIMESTAMPTZ NOT NULL,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (module_id, version)
);

CREATE INDEX IF NOT EXISTS idx_privacy_notices_module_id_version
    ON privacy_notices (module_id, version);
CREATE INDEX IF NOT EXISTS idx_privacy_notices_module_id_effective_from
    ON privacy_notices (module_id, effective_from);

-- ---------------------------------------------------------------------------
-- Append-only trail of privacy actions (redaction-policy change,
-- anonymization, consent change, erasure step, visibility
-- change, notice publication). The module also emits every one
-- of these on the hook bus for the audit module; this table is
-- the persistence target for the local trail.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS privacy_audit_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    resource TEXT NOT NULL,
    details JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_privacy_audit_entries_actor
    ON privacy_audit_entries (actor);
CREATE INDEX IF NOT EXISTS idx_privacy_audit_entries_action
    ON privacy_audit_entries (action);
CREATE INDEX IF NOT EXISTS idx_privacy_audit_entries_created_at
    ON privacy_audit_entries (created_at);
