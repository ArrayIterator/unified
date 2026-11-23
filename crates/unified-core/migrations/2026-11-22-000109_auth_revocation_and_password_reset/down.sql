-- Down migration for 2026-11-22-000109_auth_revocation_and_password_reset:
-- drop the password-reset and revocation tables.
DROP TABLE IF EXISTS password_reset_tokens;
DROP TABLE IF EXISTS auth_revoked_tokens;
