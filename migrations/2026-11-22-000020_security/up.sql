-- Security-hardening tables for the Unified Pentagonal Project
-- (security.rs, `crate::core::hardening`): the IP allowlist /
-- blocklist, IP reputation records, and failed-auth clusters.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `security_ip_lists` is keyed by (list, cidr): one entry per
--     CIDR per list (`allow` or `block`). `network` and `prefix`
--     are the parsed form of the CIDR, stored so the middleware
--     can match without re-parsing; they must agree with `cidr`.
--   - `security_reputation` is keyed by `ip`: one score per IP,
--     with a manual `flagged` marker and a reason.
--   - `security_failed_auth_clusters` is keyed by `key` (the
--     `ip + account` cluster key): repeated failures grouped by IP
--     and account, with first / last seen and the lockout flag.
--   - Deliberately NOT persisted: the brute-force attempt windows
--     (`AttemptState`, keyed by `Instant`), the fixed-window rate
--     counters (`WindowBucket`, `BurstBucket`), and the
--     concurrent-connection counters. They are sub-second,
--     process-local enforcement state; persisting them would be
--     wrong (an `Instant` cannot survive a restart, and a stale
--     window would lock users out after a deploy). The lockout
--     *status* view (`LockoutStatus`) and the failed-login summary
--     (`FailedLoginEntry`) are derived from the tables above plus
--     live counters, so they need no table of their own.
--   - The password policy (`PasswordPolicy`) is compile-time
--     constants in security.rs, not a stored setting; no table.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One IP list record: a CIDR on the allowlist or the blocklist.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS security_ip_lists (
    list VARCHAR NOT NULL CHECK (list IN ('allow', 'block')),
    cidr VARCHAR NOT NULL,
    network VARCHAR NOT NULL,
    prefix INTEGER NOT NULL CHECK (prefix >= 0 AND prefix <= 128),
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (list, cidr)
);

CREATE INDEX IF NOT EXISTS idx_security_ip_lists_list
    ON security_ip_lists (list);

-- ---------------------------------------------------------------------------
-- One IP reputation record: a score, a manual flag, and a reason.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS security_reputation (
    ip VARCHAR PRIMARY KEY,
    score INTEGER NOT NULL DEFAULT 0,
    flagged BOOLEAN NOT NULL DEFAULT FALSE,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_security_reputation_flagged
    ON security_reputation (flagged);
CREATE INDEX IF NOT EXISTS idx_security_reputation_score
    ON security_reputation (score);

-- ---------------------------------------------------------------------------
-- One failed-auth cluster: repeated failures grouped by IP and
-- account.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS security_failed_auth_clusters (
    key VARCHAR PRIMARY KEY,
    ip VARCHAR,
    account VARCHAR,
    count BIGINT NOT NULL DEFAULT 0 CHECK (count >= 0),
    first_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_security_failed_auth_clusters_ip
    ON security_failed_auth_clusters (ip);
CREATE INDEX IF NOT EXISTS idx_security_failed_auth_clusters_account
    ON security_failed_auth_clusters (account);
CREATE INDEX IF NOT EXISTS idx_security_failed_auth_clusters_locked
    ON security_failed_auth_clusters (locked);
