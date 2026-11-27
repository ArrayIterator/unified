-- Document Vault tables for the Unified Pentagonal Project
-- (`src/modules/vault.rs`): folders, documents (metadata only -
-- file bytes live in the storage module), document versions,
-- append-only access log, share links, and access requests.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/vault.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `vault_folders.owner_id` and `vault_documents.owner_id`
--     (and `vault_access_log.actor_id`,
--     `vault_access_requests.requester_id` /
--     `vault_access_requests.decided_by`) are plain VARCHAR user
--     ids: the module identifies callers by the `X-User-Id` header
--     and keeps user ids as strings, exactly like
--     `store_orders.user_id` in migration 000002. No foreign key
--     to `users` is stored here. `company_id` is likewise a plain
--     VARCHAR reference to the company module.
--   - `vault_documents` holds metadata only: `storage_target` +
--     `storage_key` reference the file bytes in the storage
--     module, `checksum_sha256` is 64 lowercase hex characters
--     (validated in the module, enforced by the CHECK below), and
--     `size_bytes` is never negative. `status` is `active` or
--     `archived`. `shared_with_client_ids` is a TEXT array of
--     client user ids the document is shared with.
--   - `vault_document_versions.version_number` is 1-based and
--     strictly increasing per document; every upload creates a new
--     version and old versions are kept (restore points the
--     document back at an old version's storage reference).
--   - `vault_access_log` is append-only by design: rows are never
--     updated or deleted by the module. `action` is one of
--     `view`, `download`, `share`, `request`, `approve`, `deny`,
--     `restore`, `delete`, `upload`, `link_revoke`, `flag_review`.
--   - `vault_share_links.token` is unique and unguessable (two
--     UUIDv7 values as hex in the module); the token is never
--     written to logs. `expires_at` is required, `max_downloads`
--     is always >= 1, and `download_count` never exceeds
--     `max_downloads` at write time in the module (the CHECK
--     below enforces it as a second line of defence).
--   - `vault_access_requests.status` is `pending`, `approved`, or
--     `denied`; approval grants visibility through the module's
--     visibility rule.
--   - Retention is flag-only, never auto-delete:
--     `vault_folders.retention_days` marks how long documents are
--     kept before they are flagged for review, and
--     `vault_folders.auto_delete` is always `false` in the module
--     (the CHECK below enforces it as a second line of defence).
--     `vault_documents.flagged_for_review` records the flag.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One folder. A top-level folder has parent_id NULL.
CREATE TABLE IF NOT EXISTS vault_folders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    parent_id UUID REFERENCES vault_folders (id) ON DELETE RESTRICT,
    owner_id VARCHAR(255) NOT NULL,
    company_id VARCHAR(255),
    retention_days BIGINT CHECK (retention_days IS NULL OR retention_days >= 0),
    auto_delete BOOLEAN NOT NULL DEFAULT FALSE CHECK (auto_delete = FALSE),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE INDEX IF NOT EXISTS idx_vault_folders_parent_id ON vault_folders (parent_id);
CREATE INDEX IF NOT EXISTS idx_vault_folders_owner_id ON vault_folders (owner_id);
CREATE INDEX IF NOT EXISTS idx_vault_folders_company_id ON vault_folders (company_id);

-- One document (metadata only).
CREATE TABLE IF NOT EXISTS vault_documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folder_id UUID NOT NULL REFERENCES vault_folders (id) ON DELETE RESTRICT,
    name VARCHAR(200) NOT NULL,
    description TEXT,
    owner_id VARCHAR(255) NOT NULL,
    company_id VARCHAR(255),
    current_version_id UUID,
    storage_target VARCHAR(128) NOT NULL,
    storage_key VARCHAR(1024) NOT NULL,
    checksum_sha256 CHAR(64) NOT NULL CHECK (checksum_sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes BIGINT NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    mime_type VARCHAR(128) NOT NULL DEFAULT 'application/octet-stream',
    shared_with_client_ids TEXT[] NOT NULL DEFAULT '{}',
    status VARCHAR(20) NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    flagged_for_review BOOLEAN NOT NULL DEFAULT FALSE,
    flagged_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_vault_documents_folder_id ON vault_documents (folder_id);
CREATE INDEX IF NOT EXISTS idx_vault_documents_owner_id ON vault_documents (owner_id);
CREATE INDEX IF NOT EXISTS idx_vault_documents_company_id ON vault_documents (company_id);
CREATE INDEX IF NOT EXISTS idx_vault_documents_status ON vault_documents (status);
CREATE INDEX IF NOT EXISTS idx_vault_documents_flagged ON vault_documents (flagged_for_review);
CREATE INDEX IF NOT EXISTS idx_vault_documents_shared_clients ON vault_documents USING GIN (shared_with_client_ids);

-- One version of a document.
CREATE TABLE IF NOT EXISTS vault_document_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES vault_documents (id) ON DELETE CASCADE,
    version_number BIGINT NOT NULL CHECK (version_number >= 1),
    storage_target VARCHAR(128) NOT NULL,
    storage_key VARCHAR(1024) NOT NULL,
    checksum_sha256 CHAR(64) NOT NULL CHECK (checksum_sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes BIGINT NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    mime_type VARCHAR(128) NOT NULL DEFAULT 'application/octet-stream',
    uploaded_by VARCHAR(255) NOT NULL,
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (document_id, version_number)
);

CREATE INDEX IF NOT EXISTS idx_vault_document_versions_document_id ON vault_document_versions (document_id);

-- The current version of a document must be one of its versions.
-- (Enforced in the module; a deferred FK cannot reference the
-- versions table before it exists, so the link is kept loose here
-- and the module guarantees it.)
ALTER TABLE vault_documents
    ADD CONSTRAINT fk_vault_documents_current_version
    FOREIGN KEY (current_version_id) REFERENCES vault_document_versions (id) ON DELETE SET NULL
    NOT VALID;

-- One append-only access-log row.
CREATE TABLE IF NOT EXISTS vault_access_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES vault_documents (id) ON DELETE CASCADE,
    actor_id VARCHAR(255) NOT NULL,
    action VARCHAR(32) NOT NULL CHECK (action IN (
        'view', 'download', 'share', 'request', 'approve', 'deny',
        'restore', 'delete', 'upload', 'link_revoke', 'flag_review'
    )),
    detail TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_vault_access_log_document_id ON vault_access_log (document_id);
CREATE INDEX IF NOT EXISTS idx_vault_access_log_actor_id ON vault_access_log (actor_id);
CREATE INDEX IF NOT EXISTS idx_vault_access_log_action ON vault_access_log (action);
CREATE INDEX IF NOT EXISTS idx_vault_access_log_created_at ON vault_access_log (created_at);

-- One share link.
CREATE TABLE IF NOT EXISTS vault_share_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES vault_documents (id) ON DELETE CASCADE,
    token VARCHAR(128) NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    max_downloads BIGINT NOT NULL DEFAULT 1 CHECK (max_downloads >= 1 AND max_downloads <= 10000),
    download_count BIGINT NOT NULL DEFAULT 0 CHECK (download_count >= 0 AND download_count <= max_downloads),
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_vault_share_links_document_id ON vault_share_links (document_id);
CREATE INDEX IF NOT EXISTS idx_vault_share_links_expires_at ON vault_share_links (expires_at);

-- One access request.
CREATE TABLE IF NOT EXISTS vault_access_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES vault_documents (id) ON DELETE CASCADE,
    requester_id VARCHAR(255) NOT NULL,
    reason TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied')),
    decided_by VARCHAR(255),
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_vault_access_requests_document_id ON vault_access_requests (document_id);
CREATE INDEX IF NOT EXISTS idx_vault_access_requests_requester_id ON vault_access_requests (requester_id);
CREATE INDEX IF NOT EXISTS idx_vault_access_requests_status ON vault_access_requests (status);
