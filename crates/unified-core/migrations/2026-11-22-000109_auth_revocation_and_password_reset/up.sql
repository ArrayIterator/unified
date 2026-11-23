-- Auth hardening (inspection C13, H2/H7): persistent token revocation
-- and password-reset tokens.
--
-- `auth_revoked_tokens` is the durable side of the auth manager's
-- revocation list: the in-memory DashMap stays the fast path, but
-- every revoke also writes a row here, and the rows are hydrated
-- back into memory after a restart, so a revoked token stays
-- revoked. Rows are pruned by expiry (a revoked token cannot be
-- used after its expiry anyway).
--
-- `password_reset_tokens` holds single-use password-reset tokens:
-- only the HMAC-SHA256 hash of the token is stored (keyed by the
-- application signing material, domain-separated), so a stolen
-- table yields no usable reset link. A token is valid while
-- `used_at IS NULL` and `expires_at` is in the future; redeeming
-- one marks it used and invalidates every sibling token for the
-- same user.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (same as migration 000095_exams: the
-- in-memory DashMap store is still authoritative for the live
-- revocation check; these tables are the persistence target):
--   - `auth_revoked_tokens.user_id` is plain TEXT with NO foreign
--     key on purpose: the auth manager identifies callers by the
--     JWT `sub` claim, which is a user id rendered as text.
--   - `password_reset_tokens.user_id` IS a UUID foreign key to
--     `users(id)`: a reset token is meaningless without its user
--     row, and the users table predates this migration.
--   - The migration has never been executed against a live
--     PostgreSQL database.

CREATE TABLE IF NOT EXISTS auth_revoked_tokens (
    jti TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_auth_revoked_tokens_expires_at
    ON auth_revoked_tokens (expires_at);
CREATE INDEX IF NOT EXISTS ix_auth_revoked_tokens_user_id
    ON auth_revoked_tokens (user_id);

CREATE TABLE IF NOT EXISTS password_reset_tokens (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS ix_password_reset_tokens_token_hash
    ON password_reset_tokens (token_hash);
CREATE INDEX IF NOT EXISTS ix_password_reset_tokens_user_id
    ON password_reset_tokens (user_id);
CREATE INDEX IF NOT EXISTS ix_password_reset_tokens_expires_at
    ON password_reset_tokens (expires_at);
