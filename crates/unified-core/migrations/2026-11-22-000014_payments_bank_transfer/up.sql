-- Payment gateway and bank-transfer tables for the Unified
-- Pentagonal Project (payments.rs, bank_transfer.rs): gateway
-- connections, intents, transactions, tokenized payment methods,
-- subscriptions, split bills, bank accounts, and manual transfer
-- verifications with their audit trail.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - Money in this migration is integer minor units (BIGINT) plus
--     an ISO 4217 currency code, exactly as the payments module
--     keeps it. That is the module's money representation, not
--     DOUBLE PRECISION, so the NUMERIC(19,4) rule for decimal
--     money (see migration 000003) does not apply here; no amount
--     in this migration is a decimal.
--   - `payment_connections.kind` is `xendit`, `hyperswitch`,
--     `2checkout`, `card`, `mastercard`, `google_pay`,
--     `apple_pay`, `paypal`, `stripe`, `midtrans`, `custom`, or
--     `bank_transfer`. `mode` is `sandbox` or `live`.
--   - `payment_connections.credentials` stores the credential
--     document as JSONB. The values are write-only: they are never
--     serialized into a view, never logged, and never included in
--     hook payloads. They must be encrypted before insert, the same
--     reservation as the `*_encrypted` columns of
--     `vcs_provider_settings` (no encryption helper exists yet).
--     `custom_config` is the JSON form of `CustomGatewayConfig`
--     (auth type and endpoint paths); it contains no credentials.
--   - `payment_intents.payment_method_token` is NOT stored: the
--     token is a payment reference, and the struct marks it
--     never-serialized. A payment method token is stored only on
--     `payment_methods.provider_token` (vault id / card token /
--     wallet token), never a PAN or CVV.
--   - `payment_transactions.idempotency_key` is unique per
--     connection: a repeated mutating call with the same key
--     returns the original transaction instead of charging twice.
--   - `split_bills.parts` from the `SplitBill` struct is
--     normalized into `split_parts`; the split is `active` until
--     every part is paid (`completed`) or it is cancelled, and the
--     sum of the parts always equals `total_minor`.
--   - `bank_transfer_verifications.order_id`, `.invoice_id`, and
--     `.customer_id` are VARCHAR, not foreign keys: the
--     bank-transfer module uses string order / customer ids, the
--     same reservation as the `store_orders.user_id` follow-up in
--     docs/SCHEMA.md. `split_id` / `part_id` reference the split
--     tables in this same migration.
--   - `bank_accounts.account_number` stores the full number; every
--     read goes through a masked view that exposes only the last
--     four characters. The full number is never logged and never
--     appears in a hook payload.
--   - `bank_transfer_settings` is a singleton row keyed by `key =
--     'global'`.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- A payment gateway connection: one provider kind, one mode
-- (sandbox / live), and its credentials.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS payment_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('xendit', 'hyperswitch', '2checkout', 'card', 'mastercard', 'google_pay', 'apple_pay', 'paypal', 'stripe', 'midtrans', 'custom', 'bank_transfer')),
    mode VARCHAR NOT NULL DEFAULT 'sandbox' CHECK (mode IN ('sandbox', 'live')),
    endpoint VARCHAR,
    credentials JSONB NOT NULL DEFAULT '{}'::jsonb,
    custom_config JSONB,
    delegate_connection_id UUID REFERENCES payment_connections (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_tested_at TIMESTAMPTZ,
    last_status VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_payment_connections_name
    ON payment_connections (name);
CREATE INDEX IF NOT EXISTS idx_payment_connections_kind
    ON payment_connections (kind);
CREATE INDEX IF NOT EXISTS idx_payment_connections_enabled
    ON payment_connections (enabled);

-- ---------------------------------------------------------------------------
-- A payment intent: the record a checkout creates first, then
-- confirms. The intent holds no card data, only a token reference
-- (which is not stored here).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS payment_intents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id UUID NOT NULL REFERENCES payment_connections (id) ON DELETE RESTRICT ON UPDATE NO ACTION,
    provider VARCHAR NOT NULL,
    customer_id VARCHAR NOT NULL,
    order_id VARCHAR,
    invoice_id VARCHAR,
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('requires_confirmation', 'authorized', 'captured', 'cancelled', 'failed')),
    capture_method VARCHAR NOT NULL CHECK (capture_method IN ('automatic', 'manual')),
    provider_transaction_id VARCHAR,
    fee_minor BIGINT CHECK (fee_minor >= 0),
    idempotency_key VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_payment_intents_connection_idempotency
    ON payment_intents (connection_id, idempotency_key);
CREATE INDEX IF NOT EXISTS idx_payment_intents_connection_id
    ON payment_intents (connection_id);
CREATE INDEX IF NOT EXISTS idx_payment_intents_customer_id
    ON payment_intents (customer_id);
CREATE INDEX IF NOT EXISTS idx_payment_intents_order_id
    ON payment_intents (order_id);
CREATE INDEX IF NOT EXISTS idx_payment_intents_status
    ON payment_intents (status);

-- ---------------------------------------------------------------------------
-- One money movement recorded by the gateway: charge, authorize,
-- capture, refund, void, payout, or subscription period charge.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS payment_transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    intent_id UUID REFERENCES payment_intents (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    connection_id UUID NOT NULL REFERENCES payment_connections (id) ON DELETE RESTRICT ON UPDATE NO ACTION,
    provider VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('charge', 'authorize', 'capture', 'refund', 'void', 'payout', 'subscription_charge')),
    customer_id VARCHAR NOT NULL,
    order_id VARCHAR,
    invoice_id VARCHAR,
    -- References gateway_subscriptions; the foreign key is added
    -- by ALTER TABLE at the end of this file, after that table
    -- exists (gateway_subscriptions is created below).
    subscription_id UUID,
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency VARCHAR NOT NULL,
    fee_minor BIGINT CHECK (fee_minor >= 0),
    status VARCHAR NOT NULL CHECK (status IN ('requires_confirmation', 'authorized', 'captured', 'pending', 'failed', 'refunded', 'partially_refunded', 'voided', 'chargeback', 'cancelled')),
    provider_transaction_id VARCHAR NOT NULL,
    provider_refund_id VARCHAR,
    idempotency_key VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_payment_transactions_connection_idempotency
    ON payment_transactions (connection_id, idempotency_key);
CREATE INDEX IF NOT EXISTS idx_payment_transactions_intent_id
    ON payment_transactions (intent_id);
CREATE INDEX IF NOT EXISTS idx_payment_transactions_connection_id
    ON payment_transactions (connection_id);
CREATE INDEX IF NOT EXISTS idx_payment_transactions_customer_id
    ON payment_transactions (customer_id);
CREATE INDEX IF NOT EXISTS idx_payment_transactions_order_id
    ON payment_transactions (order_id);
CREATE INDEX IF NOT EXISTS idx_payment_transactions_subscription_id
    ON payment_transactions (subscription_id);
CREATE INDEX IF NOT EXISTS idx_payment_transactions_status
    ON payment_transactions (status);

-- ---------------------------------------------------------------------------
-- A tokenized payment method for one customer. The record holds the
-- provider token, the card brand, the last four digits, and the
-- expiry -- never a PAN or CVV.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS payment_methods (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id VARCHAR NOT NULL,
    connection_id UUID NOT NULL REFERENCES payment_connections (id) ON DELETE RESTRICT ON UPDATE NO ACTION,
    provider VARCHAR NOT NULL,
    provider_token VARCHAR NOT NULL,
    brand VARCHAR NOT NULL,
    last4 VARCHAR NOT NULL,
    exp_month INTEGER NOT NULL CHECK (exp_month BETWEEN 1 AND 12),
    exp_year INTEGER NOT NULL CHECK (exp_year >= 2000),
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_payment_methods_customer_id
    ON payment_methods (customer_id);
CREATE INDEX IF NOT EXISTS idx_payment_methods_connection_id
    ON payment_methods (connection_id);

-- ---------------------------------------------------------------------------
-- A gateway subscription: native provider subscriptions store the
-- provider subscription id; local-schedule providers store the id
-- of the initial charge and renew by charging the stored token
-- each period.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS gateway_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id UUID NOT NULL REFERENCES payment_connections (id) ON DELETE RESTRICT ON UPDATE NO ACTION,
    provider VARCHAR NOT NULL,
    customer_id VARCHAR NOT NULL,
    order_id VARCHAR,
    plan_id VARCHAR,
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency VARCHAR NOT NULL,
    interval VARCHAR NOT NULL,
    interval_count INTEGER NOT NULL DEFAULT 1 CHECK (interval_count >= 1),
    status VARCHAR NOT NULL CHECK (status IN ('pending', 'active', 'past_due', 'cancelled', 'ended')),
    provider_subscription_id VARCHAR NOT NULL,
    payment_method_id UUID REFERENCES payment_methods (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    current_period_end TIMESTAMPTZ,
    next_renewal_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_gateway_subscriptions_connection_id
    ON gateway_subscriptions (connection_id);
CREATE INDEX IF NOT EXISTS idx_gateway_subscriptions_customer_id
    ON gateway_subscriptions (customer_id);
CREATE INDEX IF NOT EXISTS idx_gateway_subscriptions_status
    ON gateway_subscriptions (status);
CREATE INDEX IF NOT EXISTS idx_gateway_subscriptions_provider_subscription_id
    ON gateway_subscriptions (provider_subscription_id);

-- ---------------------------------------------------------------------------
-- A split bill: one order / invoice total divided into parts.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS split_bills (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id VARCHAR,
    invoice_id VARCHAR,
    customer_id VARCHAR NOT NULL,
    total_minor BIGINT NOT NULL CHECK (total_minor >= 0),
    currency VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('active', 'completed', 'cancelled')),
    paid_minor BIGINT NOT NULL DEFAULT 0 CHECK (paid_minor >= 0),
    remaining_minor BIGINT NOT NULL DEFAULT 0 CHECK (remaining_minor >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_split_bills_order_id
    ON split_bills (order_id);
CREATE INDEX IF NOT EXISTS idx_split_bills_customer_id
    ON split_bills (customer_id);
CREATE INDEX IF NOT EXISTS idx_split_bills_status
    ON split_bills (status);

-- ---------------------------------------------------------------------------
-- One part of a split bill. A `bank_transfer` part opens a manual
-- verification in the bank-transfer module instead of charging.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS split_parts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    split_id UUID NOT NULL REFERENCES split_bills (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    index INTEGER NOT NULL CHECK (index >= 0),
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency VARCHAR NOT NULL,
    provider VARCHAR NOT NULL,
    connection_id UUID REFERENCES payment_connections (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    payer_id VARCHAR NOT NULL,
    due_date TIMESTAMPTZ,
    status VARCHAR NOT NULL CHECK (status IN ('pending', 'paid', 'failed', 'cancelled')),
    paid_transaction_id UUID REFERENCES payment_transactions (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    paid_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_split_parts_split_index
    ON split_parts (split_id, index);
CREATE INDEX IF NOT EXISTS idx_split_parts_split_id
    ON split_parts (split_id);
CREATE INDEX IF NOT EXISTS idx_split_parts_status
    ON split_parts (status);

-- ---------------------------------------------------------------------------
-- One bank account that can receive transfers. The full account
-- number is stored on the record but is never returned in full:
-- every read goes through a masked view.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS bank_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bank_name VARCHAR NOT NULL,
    account_holder VARCHAR NOT NULL,
    account_number VARCHAR NOT NULL,
    swift_bic VARCHAR,
    currency VARCHAR NOT NULL,
    instructions TEXT,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_bank_accounts_active
    ON bank_accounts (active);

-- ---------------------------------------------------------------------------
-- One manual transfer verification, keyed by order (and, for split
-- bills, by split part). This record is the pending-verification
-- state for the order until the store gains its own
-- `pending_verification` status. `proof` is the submitted proof
-- document, stored as JSON; `amount_check` is `not_checked`,
-- `matched`, `mismatch`, or `skipped_no_expected_amount`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS bank_transfer_verifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id VARCHAR NOT NULL,
    invoice_id VARCHAR,
    customer_id VARCHAR NOT NULL,
    split_id UUID REFERENCES split_bills (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    part_id UUID REFERENCES split_parts (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    bank_account_id UUID REFERENCES bank_accounts (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    status VARCHAR NOT NULL CHECK (status IN ('awaiting_proof', 'submitted', 'approved', 'rejected', 'cancelled')),
    expected_amount_minor BIGINT CHECK (expected_amount_minor >= 0),
    currency VARCHAR NOT NULL,
    proof JSONB,
    amount_check VARCHAR NOT NULL DEFAULT 'not_checked' CHECK (amount_check IN ('not_checked', 'matched', 'mismatch', 'skipped_no_expected_amount')),
    amount_difference_minor BIGINT,
    transaction_id VARCHAR,
    verified_by VARCHAR,
    verified_at TIMESTAMPTZ,
    rejection_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_bank_transfer_verifications_order_id
    ON bank_transfer_verifications (order_id);
CREATE INDEX IF NOT EXISTS idx_bank_transfer_verifications_customer_id
    ON bank_transfer_verifications (customer_id);
CREATE INDEX IF NOT EXISTS idx_bank_transfer_verifications_split_id
    ON bank_transfer_verifications (split_id);
CREATE INDEX IF NOT EXISTS idx_bank_transfer_verifications_status
    ON bank_transfer_verifications (status);

-- ---------------------------------------------------------------------------
-- One audit entry of the manual checking workflow: every step
-- (created, proof requested / submitted, verified, rejected,
-- cancelled) with the actor, the actor role, an optional reason,
-- and a timestamp.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS bank_transfer_verification_audit (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    verification_id UUID NOT NULL REFERENCES bank_transfer_verifications (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    action VARCHAR NOT NULL CHECK (action IN ('created', 'proof_requested', 'proof_submitted', 'verified', 'rejected', 'cancelled')),
    actor VARCHAR NOT NULL,
    actor_role VARCHAR NOT NULL CHECK (actor_role IN ('customer', 'admin', 'system')),
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_bank_transfer_verification_audit_verification_id
    ON bank_transfer_verification_audit (verification_id);

-- ---------------------------------------------------------------------------
-- Operator settings for manual verification (singleton row).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS bank_transfer_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    amount_tolerance_minor BIGINT NOT NULL DEFAULT 0 CHECK (amount_tolerance_minor >= 0),
    proof_expiry_days INTEGER NOT NULL DEFAULT 7 CHECK (proof_expiry_days >= 0),
    require_reference_number BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Deferred foreign key: `payment_transactions.subscription_id`
-- references `gateway_subscriptions`, which is created after
-- `payment_transactions` in this file.
ALTER TABLE payment_transactions
    ADD CONSTRAINT fk_payment_transactions_subscription_id
    FOREIGN KEY (subscription_id) REFERENCES gateway_subscriptions (id) ON DELETE SET NULL ON UPDATE NO ACTION;
