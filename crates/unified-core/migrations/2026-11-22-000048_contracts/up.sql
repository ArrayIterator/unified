-- Contracts & E-Signature tables for the Unified Pentagonal Project
-- (`src/modules/contracts.rs`): contract templates with
-- placeholders, contracts with a validated status machine,
-- signers with hashed signing tokens, and a full audit trail.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/contracts.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `contract_templates.body` holds the template text with
--     `{{placeholder}}` markers; `placeholders` is the closed list
--     of names the body may use. The module rejects any marker in
--     the body that is not declared, at save time.
--   - `contracts.status` is `draft`, `sent`, `partially_signed`,
--     `signed`, `active`, `expired`, `terminated`, or `voided`
--     (`ContractStatus`). The module derives `expired` at read time
--     for an `active` contract whose end date passed; the stored
--     status never moves to `expired` on its own - the scheduler
--     listener or the check-expiry endpoint materialises it.
--   - `contracts.value` is NUMERIC(19,4) (money), never negative.
--     `currency` is a 3-letter ISO code.
--   - `contracts.content_hash` is the SHA-256 hash of the sealed
--     content, recorded when the last signer signs. While set, the
--     module rejects edits; the audited escape hatch is voiding
--     the contract and renewing it into a new draft.
--   - `contracts.renewed_from_id` links a renewal draft to the
--     contract it was renewed from. `alert_days` holds the expiry
--     alert offsets (days before `ends_at`); `expiry_alert_status`
--     is `not_scheduled`, `queued`, or `declined` (the scheduler
--     does not know the `contracts.expiry_alert` job kind yet).
--   - `contract_signers.token_hash` stores only the SHA-256 hash
--     of the signing token - the raw token is returned once, at
--     creation and at resend, and is never persisted. The hash is
--     unique so a token maps to exactly one signer. A signer email
--     is unique per contract.
--   - `contract_audit_trail.ip_hash` and `user_agent_hash` are
--     SHA-256 hex digests; the raw IP address and raw user agent
--     are never stored. `actor` is the `X-User-Id` header value,
--     the signer email for token-gated actions, or `scheduler`.
--   - `contracts.template_id` and `contracts.renewed_from_id`
--     are plain references to rows in this same migration;
--     `created_by` is a plain VARCHAR user id, exactly like
--     `store_orders.user_id` in migration 000002. No foreign key
--     to `users` is stored here.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One contract template.
CREATE TABLE IF NOT EXISTS contract_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    body TEXT NOT NULL,
    placeholders TEXT[] NOT NULL DEFAULT '{}',
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_contract_templates_name ON contract_templates (name);

-- One contract.
CREATE TABLE IF NOT EXISTS contracts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID REFERENCES contract_templates (id) ON DELETE SET NULL,
    title VARCHAR(200) NOT NULL,
    company_party VARCHAR(200) NOT NULL,
    counterparty_name VARCHAR(200) NOT NULL,
    counterparty_email VARCHAR(254) NOT NULL,
    terms TEXT NOT NULL,
    value NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (value >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    signing_order VARCHAR(20) NOT NULL DEFAULT 'sequential'
        CHECK (signing_order IN ('sequential', 'parallel')),
    status VARCHAR(20) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'sent', 'partially_signed', 'signed', 'active', 'expired', 'terminated', 'voided')),
    starts_at TIMESTAMPTZ,
    ends_at TIMESTAMPTZ,
    signed_at TIMESTAMPTZ,
    content_hash VARCHAR(64),
    void_reason TEXT,
    terminate_reason TEXT,
    renewed_from_id UUID REFERENCES contracts (id) ON DELETE SET NULL,
    alert_days INTEGER[] NOT NULL DEFAULT '{}',
    expiry_alert_status VARCHAR(20) NOT NULL DEFAULT 'not_scheduled'
        CHECK (expiry_alert_status IN ('not_scheduled', 'queued', 'declined')),
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (starts_at IS NULL OR ends_at IS NULL OR ends_at > starts_at)
);
CREATE INDEX IF NOT EXISTS idx_contracts_status ON contracts (status);
CREATE INDEX IF NOT EXISTS idx_contracts_counterparty_email ON contracts (counterparty_email);
CREATE INDEX IF NOT EXISTS idx_contracts_template_id ON contracts (template_id);
CREATE INDEX IF NOT EXISTS idx_contracts_renewed_from_id ON contracts (renewed_from_id);
CREATE INDEX IF NOT EXISTS idx_contracts_ends_at ON contracts (ends_at);

-- One signer of a contract.
CREATE TABLE IF NOT EXISTS contract_signers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_id UUID NOT NULL REFERENCES contracts (id) ON DELETE CASCADE,
    name VARCHAR(200) NOT NULL,
    email VARCHAR(254) NOT NULL,
    order_index INTEGER NOT NULL DEFAULT 0 CHECK (order_index >= 0),
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'viewed', 'signed', 'declined')),
    token_hash VARCHAR(64) NOT NULL UNIQUE,
    token_expires_at TIMESTAMPTZ NOT NULL,
    signed_at TIMESTAMPTZ,
    declined_reason TEXT,
    viewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (contract_id, email)
);
CREATE INDEX IF NOT EXISTS idx_contract_signers_contract_id ON contract_signers (contract_id);
CREATE INDEX IF NOT EXISTS idx_contract_signers_token_hash ON contract_signers (token_hash);
CREATE INDEX IF NOT EXISTS idx_contract_signers_status ON contract_signers (status);

-- One audit-trail entry for a contract.
CREATE TABLE IF NOT EXISTS contract_audit_trail (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_id UUID NOT NULL REFERENCES contracts (id) ON DELETE CASCADE,
    signer_id UUID REFERENCES contract_signers (id) ON DELETE SET NULL,
    actor VARCHAR(255) NOT NULL,
    action VARCHAR(100) NOT NULL,
    ip_hash VARCHAR(64) NOT NULL,
    user_agent_hash VARCHAR(64) NOT NULL,
    details TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_contract_audit_trail_contract_id ON contract_audit_trail (contract_id);
CREATE INDEX IF NOT EXISTS idx_contract_audit_trail_created_at ON contract_audit_trail (created_at);
CREATE INDEX IF NOT EXISTS idx_contract_audit_trail_action ON contract_audit_trail (action);
