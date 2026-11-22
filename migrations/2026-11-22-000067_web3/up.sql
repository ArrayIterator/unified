-- Web3 / Blockchain module tables for the Unified Pentagonal Project
-- (`src/modules/web3.rs`): chain networks, watched addresses,
-- observed on-chain transactions, on-chain anchors of record hashes,
-- read-only linked wallets, balance snapshots, and module settings.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
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
--     truth for identity; wallets and watched addresses here only
--     reference users by id and never duplicate user rows.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE: deleting a company
--     removes its watched addresses, anchors, wallets, and settings
--     rows. `web3_settings` is one row per company scope plus the
--     global row (`UNIQUE (scope)`).
--   - Money is NUMERIC(19,4): `web3_transactions.amount` and
--     `web3_balances` has no money column (see below). Human-readable
--     chain amounts that fit are stored in `amount` NUMERIC(19,4);
--     the exact chain-native amount is always stored in
--     `amount_raw` TEXT (wei, stroops, lovelace, or the asset's
--     smallest unit as a decimal string), because chain-native
--     amounts routinely exceed NUMERIC(19,4) and must never lose
--     precision to a float. `amount` is a display convenience only;
--     `amount_raw` is authoritative.
--   - `web3_networks.api_key_encrypted` and
--     `web3_settings.anchoring_key_encrypted` hold ciphertext only
--     (write-only secrets, encrypted with a key derived from
--     `app.secret_key`). The plaintext key is never stored, never
--     returned, and never logged. There is NO private-key column
--     anywhere in this schema: the server never stores a user
--     private key and never signs transactions with user funds.
--     Anchoring uses a dedicated anchoring-only service key, stored
--     write-only in `web3_settings.anchoring_key_encrypted`.
--   - `web3_anchors.content_sha256` is the SHA-256 hex of a Unified
--     record. Only the hash is ever anchored on-chain; record
--     contents stay off-chain at all times.
--   - `web3_wallets.challenge_nonce_hash` is the SHA-256 hex of the
--     outstanding challenge nonce. The raw nonce lives only in the
--     in-memory challenge store and is never persisted.
--   - `web3_transactions` is deduplicated by
--     `UNIQUE (network_id, tx_hash)`: one row per on-chain
--     transaction per network. `web3_wallets` is deduplicated by
--     `UNIQUE (user_id, network_id, address)`: one link per user per
--     address per network. `web3_watched_addresses` is deduplicated
--     by `UNIQUE (network_id, address, user_id)`.
--   - `web3_transactions.watched_address_id` references
--     `web3_watched_addresses (id)` ON DELETE SET NULL so deleting
--     a watched address keeps the observed transaction history.
--   - `web3_balances.wallet_id` references `web3_wallets (id)` ON
--     DELETE CASCADE: unlinking a wallet removes its snapshots.
--   - `web3_anchors.network_id`, `web3_transactions.network_id`,
--     `web3_watched_addresses.network_id`, `web3_wallets.network_id`,
--     and `web3_balances.network_id` reference
--     `web3_networks (id)` ON DELETE CASCADE, except
--     `web3_anchors.network_id`, which is ON DELETE RESTRICT: an
--     anchor is tamper-evident proof, and its network must not
--     disappear while anchors still reference it (delete the
--     anchors first, or keep the network row).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One configured chain network: EVM (generic JSON-RPC), Stellar
-- (Horizon), Cardano (Blockfrost- or Koios-style REST), or generic
-- (configuration only, no provider implementation). Endpoint URLs are
-- validated against an allowlist before they are stored; a localhost
-- or private endpoint is only stored when an admin explicitly
-- allowlisted it (`allow_private_endpoint`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS web3_networks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('evm', 'stellar', 'cardano', 'generic')),
    endpoint_url TEXT NOT NULL,
    chain_id BIGINT CHECK (chain_id IS NULL OR chain_id >= 0),
    native_asset_symbol TEXT NOT NULL CHECK (native_asset_symbol ~ '^[A-Z0-9]{1,16}$'),
    explorer_url_template TEXT,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    indexer_url TEXT,
    verify_endpoint TEXT,
    api_key_encrypted TEXT,
    allow_private_endpoint BOOLEAN NOT NULL DEFAULT FALSE,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind <> 'evm' OR chain_id IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_web3_networks_kind_enabled
    ON web3_networks (kind, enabled);

-- ---------------------------------------------------------------------------
-- One address watched for incoming and outgoing transactions, owned
-- by one user (optionally inside one company).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS web3_watched_addresses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    network_id UUID NOT NULL REFERENCES web3_networks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    address TEXT NOT NULL,
    label TEXT,
    user_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    last_checked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (network_id, address, user_id)
);

CREATE INDEX IF NOT EXISTS idx_web3_watched_addresses_user_id
    ON web3_watched_addresses (user_id);
CREATE INDEX IF NOT EXISTS idx_web3_watched_addresses_network_id
    ON web3_watched_addresses (network_id);
CREATE INDEX IF NOT EXISTS idx_web3_watched_addresses_company_id
    ON web3_watched_addresses (company_id);

-- ---------------------------------------------------------------------------
-- One observed on-chain transaction. `amount_raw` is the exact
-- chain-native amount as a decimal string (authoritative);
-- `amount` is the human-readable decimal when it fits NUMERIC(19,4)
-- (display only). Deduplicated by (network_id, tx_hash).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS web3_transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    network_id UUID NOT NULL REFERENCES web3_networks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    tx_hash TEXT NOT NULL,
    from_address TEXT NOT NULL,
    to_address TEXT NOT NULL,
    amount_raw TEXT NOT NULL,
    amount NUMERIC(19, 4) CHECK (amount IS NULL OR amount >= 0),
    asset TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'confirmed', 'failed')),
    confirmations BIGINT NOT NULL DEFAULT 0 CHECK (confirmations >= 0),
    block_number BIGINT CHECK (block_number IS NULL OR block_number >= 0),
    direction TEXT NOT NULL DEFAULT 'observed'
        CHECK (direction IN ('incoming', 'outgoing', 'observed')),
    watched_address_id UUID REFERENCES web3_watched_addresses (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    occurred_at TIMESTAMPTZ,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (network_id, tx_hash)
);

CREATE INDEX IF NOT EXISTS idx_web3_transactions_network_id_tx_hash
    ON web3_transactions (network_id, tx_hash);
CREATE INDEX IF NOT EXISTS idx_web3_transactions_watched_address_id
    ON web3_transactions (watched_address_id);
CREATE INDEX IF NOT EXISTS idx_web3_transactions_from_address
    ON web3_transactions (from_address);
CREATE INDEX IF NOT EXISTS idx_web3_transactions_to_address
    ON web3_transactions (to_address);
CREATE INDEX IF NOT EXISTS idx_web3_transactions_status
    ON web3_transactions (status);

-- ---------------------------------------------------------------------------
-- One on-chain anchor: only the SHA-256 hash of a Unified record
-- (invoice, contract, certificate, audit entry, or any other record
-- type) is anchored. Record contents are never stored here and never
-- sent on-chain. The network reference is RESTRICT: anchors are
-- tamper-evident proof and must outlive their network row.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS web3_anchors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    record_type TEXT NOT NULL,
    record_id TEXT NOT NULL,
    content_sha256 TEXT NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    network_id UUID NOT NULL REFERENCES web3_networks (id)
        ON DELETE RESTRICT ON UPDATE NO ACTION,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'submitted', 'confirmed', 'failed')),
    tx_hash TEXT,
    confirmations BIGINT NOT NULL DEFAULT 0 CHECK (confirmations >= 0),
    block_number BIGINT CHECK (block_number IS NULL OR block_number >= 0),
    requested_by TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    submitted_at TIMESTAMPTZ,
    confirmed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_web3_anchors_record_type_record_id
    ON web3_anchors (record_type, record_id);
CREATE INDEX IF NOT EXISTS idx_web3_anchors_network_id_status
    ON web3_anchors (network_id, status);
CREATE INDEX IF NOT EXISTS idx_web3_anchors_tx_hash
    ON web3_anchors (tx_hash);
CREATE INDEX IF NOT EXISTS idx_web3_anchors_requested_by
    ON web3_anchors (requested_by);

-- ---------------------------------------------------------------------------
-- One read-only linked wallet. The server observes this address and
-- can never spend from it. `challenge_nonce_hash` is the SHA-256 hex
-- of the outstanding challenge nonce; the raw nonce is never stored.
-- `verification_status` is `unverified`, `pending`, `verified`,
-- `lookup_only` (Cardano provider lookup: the address exists, which
-- does not by itself prove control), or `failed`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS web3_wallets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    network_id UUID NOT NULL REFERENCES web3_networks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    address TEXT NOT NULL,
    label TEXT,
    verification_status TEXT NOT NULL DEFAULT 'unverified'
        CHECK (verification_status IN ('unverified', 'pending', 'verified', 'lookup_only', 'failed')),
    challenge_nonce_hash TEXT CHECK (challenge_nonce_hash IS NULL OR challenge_nonce_hash ~ '^[0-9a-f]{64}$'),
    challenge_expires_at TIMESTAMPTZ,
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, network_id, address)
);

CREATE INDEX IF NOT EXISTS idx_web3_wallets_user_id
    ON web3_wallets (user_id);
CREATE INDEX IF NOT EXISTS idx_web3_wallets_network_id
    ON web3_wallets (network_id);
CREATE INDEX IF NOT EXISTS idx_web3_wallets_verification_status
    ON web3_wallets (verification_status);

-- ---------------------------------------------------------------------------
-- One balance snapshot for a linked wallet. `amount_raw` is the exact
-- chain-native amount as a decimal string (authoritative);
-- `amount` is the human-readable decimal when it fits NUMERIC(19,4)
-- (display only). Unlinking a wallet removes its snapshots.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS web3_balances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    wallet_id UUID NOT NULL REFERENCES web3_wallets (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    network_id UUID NOT NULL REFERENCES web3_networks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    address TEXT NOT NULL,
    asset TEXT NOT NULL,
    amount_raw TEXT NOT NULL,
    amount NUMERIC(19, 4) CHECK (amount IS NULL OR amount >= 0),
    decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals >= 0 AND decimals <= 18),
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_web3_balances_wallet_id_asset
    ON web3_balances (wallet_id, asset);
CREATE INDEX IF NOT EXISTS idx_web3_balances_network_id
    ON web3_balances (network_id);

-- ---------------------------------------------------------------------------
-- Module settings (one row, scope = 'global'). The public ledger is
-- default-deny (`public_ledger_enabled = FALSE`). The anchoring relay
-- URL must pass the endpoint allowlist; the anchoring service key is
-- ciphertext only (write-only, never returned, never logged).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS web3_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope TEXT NOT NULL DEFAULT 'global',
    public_ledger_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    allowed_private_hosts TEXT[] NOT NULL DEFAULT '{}',
    allowed_extra_hosts TEXT[] NOT NULL DEFAULT '{}',
    anchoring_relay_url TEXT,
    anchoring_key_encrypted TEXT,
    max_poll_transactions INTEGER NOT NULL DEFAULT 25
        CHECK (max_poll_transactions BETWEEN 1 AND 50),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (scope)
);
