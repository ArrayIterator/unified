-- Public link sharing and auditor sandbox tables for the Unified Pentagonal Project
-- (`src/core/sharing.rs`, `src/modules/auditors.rs`): public links that
-- expose exactly one resource in one view, their access log, temporary
-- sandbox auditor accounts, their scopes, and their sessions.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `public_links.token_hash` stores ONLY the SHA-256 hex of the
--     raw token. The raw token is shown exactly once, at creation
--     time, and is never stored anywhere. `token_hash` is UNIQUE so
--     a token can never be minted twice.
--   - `public_link_access_log.ip_hash` / `user_agent_hash` store
--     ONLY SHA-256 hex digests of the client IP and user agent.
--     Raw IPs and raw user agents are never stored.
--   - `auditor_accounts.user_id` is TEXT with NO foreign key on
--     purpose: actor ids in this codebase are opaque strings (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table. The users module stays the source of
--     truth for identity; auditor accounts here only reference
--     users by id and never duplicate user rows. The same applies
--     to `auditor_accounts.auditor_of`, `revoked_by`, and
--     `auditor_sessions.revoked_by`.
--   - `auditor_accounts.password_hash` is the Argon2id PHC hash of
--     the one-time password shown once at creation. The raw
--     password is never stored.
--   - `auditor_scopes` carries the read-only scope: one row per
--     (module, resource kind, record / company) grant. There is no
--     row that means "everything": a scope must name modules
--     explicitly, and `allow_export` defaults to FALSE — exports
--     are denied unless a row explicitly grants them.
--   - `auditor_sessions` are records, not issued tokens: the auth
--     flow owns the actual session token, this table only tracks
--     liveness for the banner endpoint and for one-click revoke.
--   - Money: none here. All ids are Uuid, all timestamps are
--     Timestamptz.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One public link: exactly one resource kind, one resource id, one
-- view. Read-only by construction: the scope carries no action.
-- `expires_at` is required and at most 30 days after creation;
-- `revoked_at` set means the link never validates again.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS public_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    scope_kind TEXT NOT NULL CHECK (scope_kind ~ '^[a-z0-9_-]{1,64}$'),
    scope_id UUID NOT NULL,
    scope_view TEXT NOT NULL CHECK (scope_view ~ '^[a-z0-9_-]{1,64}$'),
    created_by TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at)
);

CREATE INDEX IF NOT EXISTS idx_public_links_token_hash
    ON public_links (token_hash);
CREATE INDEX IF NOT EXISTS idx_public_links_scope
    ON public_links (scope_kind, scope_id, scope_view);
CREATE INDEX IF NOT EXISTS idx_public_links_created_by
    ON public_links (created_by);
CREATE INDEX IF NOT EXISTS idx_public_links_expires_at
    ON public_links (expires_at);

-- ---------------------------------------------------------------------------
-- One public-link access attempt: allowed AND denied attempts are
-- both recorded. `link_id` is NULL when the presented token matched
-- no link at all. Only hashes of the IP and user agent are stored.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS public_link_access_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    link_id UUID REFERENCES public_links (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    accessed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ip_hash TEXT CHECK (ip_hash IS NULL OR ip_hash ~ '^[0-9a-f]{64}$'),
    user_agent_hash TEXT CHECK (user_agent_hash IS NULL OR user_agent_hash ~ '^[0-9a-f]{64}$'),
    allowed BOOLEAN NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('ok', 'not_found', 'bad_token', 'revoked', 'expired', 'scope_mismatch')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_public_link_access_log_link_id_accessed_at
    ON public_link_access_log (link_id, accessed_at);
CREATE INDEX IF NOT EXISTS idx_public_link_access_log_allowed
    ON public_link_access_log (allowed);
CREATE INDEX IF NOT EXISTS idx_public_link_access_log_reason
    ON public_link_access_log (reason);

-- ---------------------------------------------------------------------------
-- One temporary sandbox auditor account. Time-limited by
-- construction: `expires_at` is required. `is_auditor` is always
-- true and `auditor_of` is the admin actor id that created the
-- account, so the account is distinguishable in audit records and
-- sessions. `scheduler_status` records whether the expiry job was
-- actually queued (`queued` / `declined`); it never pretends a job
-- was queued when the scheduler declined it.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS auditor_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    label TEXT NOT NULL,
    email TEXT NOT NULL,
    is_auditor BOOLEAN NOT NULL DEFAULT TRUE,
    auditor_of TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'revoked', 'expired')),
    expires_at TIMESTAMPTZ NOT NULL,
    password_hash TEXT NOT NULL,
    revoked_at TIMESTAMPTZ,
    revoked_by TEXT,
    revoke_reason TEXT,
    scheduler_status TEXT NOT NULL DEFAULT 'declined'
        CHECK (scheduler_status IN ('queued', 'declined')),
    scheduler_job_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at),
    CHECK (status <> 'revoked' OR revoked_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_auditor_accounts_user_id
    ON auditor_accounts (user_id);
CREATE INDEX IF NOT EXISTS idx_auditor_accounts_status_expires_at
    ON auditor_accounts (status, expires_at);
CREATE INDEX IF NOT EXISTS idx_auditor_accounts_auditor_of
    ON auditor_accounts (auditor_of);

-- ---------------------------------------------------------------------------
-- One scope entry of one auditor account: what the auditor may see.
-- `resource_id` set means exactly that one record; `resource_id`
-- empty and `company_id` set means every record of this kind inside
-- that company; both empty means every record of this kind in the
-- module. `allow_export` defaults to FALSE.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS auditor_scopes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    auditor_id UUID NOT NULL REFERENCES auditor_accounts (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    module TEXT NOT NULL CHECK (module ~ '^[a-z0-9_-]{1,64}$'),
    resource_kind TEXT NOT NULL CHECK (resource_kind ~ '^[a-z0-9_-]{1,64}$'),
    resource_id UUID,
    company_id UUID,
    allow_export BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (auditor_id, module, resource_kind, resource_id, company_id, allow_export)
);

CREATE INDEX IF NOT EXISTS idx_auditor_scopes_auditor_id
    ON auditor_scopes (auditor_id);
CREATE INDEX IF NOT EXISTS idx_auditor_scopes_module_resource_kind
    ON auditor_scopes (module, resource_kind);
CREATE INDEX IF NOT EXISTS idx_auditor_scopes_company_id
    ON auditor_scopes (company_id);

-- ---------------------------------------------------------------------------
-- One recorded auditor session: a login of an auditor user id.
-- `revoked_at` set means the session was killed (account revoke,
-- expiry, or single-session revoke). Sessions are records, not
-- issued tokens: the auth flow owns the actual session token.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS auditor_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    auditor_id UUID NOT NULL REFERENCES auditor_accounts (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    revoked_by TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (revoked_at IS NULL OR revoked_at >= started_at),
    CHECK (last_seen_at >= started_at)
);

CREATE INDEX IF NOT EXISTS idx_auditor_sessions_auditor_id_started_at
    ON auditor_sessions (auditor_id, started_at);
CREATE INDEX IF NOT EXISTS idx_auditor_sessions_user_id
    ON auditor_sessions (user_id);
