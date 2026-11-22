-- Durable idempotency and webhook-event claims for the payments
-- module (`src/modules/payments/`, C5 fix 2026-11-22).
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (same as migration 000014_payments_bank_transfer:
-- the in-memory DashMap store is still authoritative):
--   - The in-memory claims (`claim_idempotency` in
--     `handlers_catalog.rs`, the webhook event-id claim in
--     `handlers_webhooks.rs`) are PER-PROCESS ONLY: two instances
--     of this service can still both claim the same key. These
--     tables are the durable form for multi-instance deployments:
--     insert the claim row first and treat a unique-violation on
--     the primary key as "already claimed", then update the row to
--     `completed` (with `transaction_id`) once the provider call
--     is recorded, or delete it when the provider call fails
--     before anything is recorded. The read path does not use
--     these tables yet; the migration has never been executed
--     against a live PostgreSQL database.
--   - `payment_idempotency_claims.transaction_id` is NULL while
--     the claim is in flight and holds the recorded transaction
--     once the provider call succeeds, so a duplicate can be
--     answered from the claim row alone.
--   - `webhook_event_claims.processed_at` is NULL while the event
--     is being processed and set on success; a row whose
--     processing failed is deleted by the receiver, so the
--     provider's retry is processed instead of dismissed as a
--     duplicate.
--   - `claimed_at` / `received_at` are indexed so a sweeper can
--     drop stale in-flight claims (a crashed worker's claim must
--     not block its key forever).

CREATE TABLE IF NOT EXISTS payment_idempotency_claims (
    connection_id UUID NOT NULL REFERENCES payment_connections (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    idempotency_key VARCHAR NOT NULL,
    transaction_id UUID,
    status VARCHAR NOT NULL DEFAULT 'claimed' CHECK (status IN ('claimed', 'completed')),
    claimed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (connection_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_payment_idempotency_claims_claimed_at
    ON payment_idempotency_claims (claimed_at);

CREATE TABLE IF NOT EXISTS webhook_event_claims (
    event_id VARCHAR PRIMARY KEY,
    provider VARCHAR NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ,
    outcome VARCHAR
);

CREATE INDEX IF NOT EXISTS idx_webhook_event_claims_received_at
    ON webhook_event_claims (received_at);
