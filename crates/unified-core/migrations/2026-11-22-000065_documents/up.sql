-- Documents module tables for the Unified Pentagonal Project
-- (`src/modules/documents.rs`): documents, document versions,
-- chunked / resumable upload sessions and their parts, per-module
-- allowlist rules, download records, and local quotas.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` / `owner_id` / `uploader_id` columns are TEXT with
--     NO foreign key on purpose: actor ids in this codebase are
--     opaque strings (the `X-User-Id` header), not necessarily the
--     UUID primary key of the `users` table. The users module stays
--     the source of truth for identity; documents here only
--     reference users by id and never duplicate user rows.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE: deleting a company
--     removes its documents, sessions, and local quotas.
--   - Content is deduplicated by SHA-256: `documents.checksum`
--     and `document_versions.checksum` are the SHA-256 hex of the
--     version's bytes, and `document_versions.storage_key` is the
--     validated storage key (`objects/<prefix>/<sha256>`) of the
--     shared object. No file bytes live in this schema.
--   - `document_versions` is append-only: restoring an old version
--     creates a new version row with the old content and never
--     mutates an existing version row.
--   - `document_upload_parts` stores metadata ONLY: the part bytes
--     live in memory until the session is completed or aborted.
--     `part_sha256` is the SHA-256 hex of that part's bytes and is
--     verified on receipt; `expected_sha256` on the session is the
--     SHA-256 hex of the complete assembled file.
--   - `document_allowlist_rules` holds the admin-managed per-module
--     allowlist: allowed extensions (lowercase, no dot), allowed
--     MIME types, and the maximum file size in bytes. A module with
--     no rule rejects uploads (fail closed).
--   - `document_downloads` is the audit trail: one row per download,
--     including downloads redeemed through a signed URL (recorded
--     with `user_id = 'signed-url'`) and downloads of files whose
--     scan was skipped (recorded with `scan_status = 'skipped'`).
--   - `document_quotas` holds the local per-company / per-user
--     quotas, used only when the Storage Management module does not
--     answer the `storage.object.upload_requested` event. A scope
--     with no row here is unlimited locally; `bytes_limit` NULL
--     also means unlimited. `used_bytes` tracks the sum of the
--     document sizes owned by that scope (logical usage, even when
--     the bytes are deduplicated into one stored object).
--   - `documents.scan_status` is one of `pending`, `clean`,
--     `infected`, `error`, `skipped`. `scan_threat` holds the
--     scanner-supplied threat label (or the scan error reason) and
--     never holds file content.
--   - Signed download URLs are NOT stored: the HMAC key is
--     per-process random (generated at startup, never persisted),
--     so no token table exists and links stop working after a
--     restart, by design.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One document: the current version plus its access scope.
-- `scope` is `owner`, `company`, or `module`.
CREATE TABLE IF NOT EXISTS documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    module_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    owner_id TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'owner'
        CHECK (scope IN ('owner', 'company', 'module')),
    mime TEXT NOT NULL,
    size BIGINT NOT NULL CHECK (size > 0 AND size <= 104857600),
    checksum TEXT NOT NULL CHECK (checksum ~ '^[0-9a-f]{64}$'),
    storage_key TEXT NOT NULL,
    current_version INTEGER NOT NULL DEFAULT 1 CHECK (current_version >= 1),
    version_count INTEGER NOT NULL DEFAULT 1 CHECK (version_count >= 1),
    scan_status TEXT NOT NULL DEFAULT 'pending'
        CHECK (scan_status IN ('pending', 'clean', 'infected', 'error', 'skipped')),
    scan_threat TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_documents_owner_id_created_at
    ON documents (owner_id, created_at);
CREATE INDEX IF NOT EXISTS idx_documents_company_id
    ON documents (company_id);
CREATE INDEX IF NOT EXISTS idx_documents_module_id
    ON documents (module_id);
CREATE INDEX IF NOT EXISTS idx_documents_checksum
    ON documents (checksum);
CREATE INDEX IF NOT EXISTS idx_documents_scan_status
    ON documents (scan_status);

-- One version of a document. History is append-only.
CREATE TABLE IF NOT EXISTS document_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES documents (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    version INTEGER NOT NULL CHECK (version >= 1),
    checksum TEXT NOT NULL CHECK (checksum ~ '^[0-9a-f]{64}$'),
    storage_key TEXT NOT NULL,
    size BIGINT NOT NULL CHECK (size > 0 AND size <= 104857600),
    mime TEXT NOT NULL,
    uploader_id TEXT NOT NULL,
    scan_status TEXT NOT NULL DEFAULT 'pending'
        CHECK (scan_status IN ('pending', 'clean', 'infected', 'error', 'skipped')),
    scan_threat TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (document_id, version)
);

CREATE INDEX IF NOT EXISTS idx_document_versions_document_id_version
    ON document_versions (document_id, version);
CREATE INDEX IF NOT EXISTS idx_document_versions_checksum
    ON document_versions (checksum);

-- One resumable upload session. `document_id` is set only when the
-- session is a re-upload of an existing document.
CREATE TABLE IF NOT EXISTS document_upload_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID REFERENCES documents (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    module_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    owner_id TEXT NOT NULL,
    total_size BIGINT NOT NULL CHECK (total_size > 0 AND total_size <= 104857600),
    expected_sha256 TEXT NOT NULL CHECK (expected_sha256 ~ '^[0-9a-f]{64}$'),
    chunk_size BIGINT NOT NULL CHECK (chunk_size >= 65536 AND chunk_size <= 8388608),
    part_count INTEGER NOT NULL CHECK (part_count >= 1),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'completed', 'aborted', 'expired')),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_document_upload_sessions_owner_id_status
    ON document_upload_sessions (owner_id, status);
CREATE INDEX IF NOT EXISTS idx_document_upload_sessions_expires_at
    ON document_upload_sessions (expires_at);
CREATE INDEX IF NOT EXISTS idx_document_upload_sessions_document_id
    ON document_upload_sessions (document_id);

-- One received chunk part (metadata only; the bytes live in memory
-- until the session completes or aborts).
CREATE TABLE IF NOT EXISTS document_upload_parts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES document_upload_sessions (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    part_index INTEGER NOT NULL CHECK (part_index >= 0),
    size BIGINT NOT NULL CHECK (size > 0 AND size <= 8388608),
    part_sha256 TEXT NOT NULL CHECK (part_sha256 ~ '^[0-9a-f]{64}$'),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (session_id, part_index)
);

CREATE INDEX IF NOT EXISTS idx_document_upload_parts_session_id_part_index
    ON document_upload_parts (session_id, part_index);

-- One per-module allowlist rule (admin-managed).
CREATE TABLE IF NOT EXISTS document_allowlist_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    module_id TEXT NOT NULL,
    extensions TEXT[] NOT NULL DEFAULT '{}',
    mime_types TEXT[] NOT NULL DEFAULT '{}',
    max_bytes BIGINT NOT NULL CHECK (max_bytes > 0 AND max_bytes <= 104857600),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (module_id)
);

CREATE INDEX IF NOT EXISTS idx_document_allowlist_rules_module_id
    ON document_allowlist_rules (module_id);

-- One download record: the audit trail, one row per download.
CREATE TABLE IF NOT EXISTS document_downloads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID REFERENCES documents (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    version INTEGER NOT NULL CHECK (version >= 1),
    user_id TEXT NOT NULL,
    via TEXT NOT NULL DEFAULT 'direct'
        CHECK (via IN ('direct', 'signed-url')),
    scan_status TEXT NOT NULL
        CHECK (scan_status IN ('pending', 'clean', 'infected', 'error', 'skipped')),
    bytes BIGINT NOT NULL CHECK (bytes >= 0),
    downloaded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_document_downloads_document_id_downloaded_at
    ON document_downloads (document_id, downloaded_at);
CREATE INDEX IF NOT EXISTS idx_document_downloads_user_id_downloaded_at
    ON document_downloads (user_id, downloaded_at);

-- One local quota row (per-company or per-user). Used only when the
-- Storage Management module does not answer the quota event.
CREATE TABLE IF NOT EXISTS document_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope TEXT NOT NULL CHECK (scope IN ('company', 'user')),
    scope_id TEXT NOT NULL,
    bytes_limit BIGINT CHECK (bytes_limit IS NULL OR bytes_limit >= 0),
    used_bytes BIGINT NOT NULL DEFAULT 0 CHECK (used_bytes >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (scope, scope_id)
);

CREATE INDEX IF NOT EXISTS idx_document_quotas_scope_scope_id
    ON document_quotas (scope, scope_id);
