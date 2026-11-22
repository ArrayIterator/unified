-- Roll back 2026-11-22-000112_payment_idempotency_claims.
-- The core payments tables from 2026-11-22-000014 are untouched.

DROP TABLE IF EXISTS webhook_event_claims;
DROP TABLE IF EXISTS payment_idempotency_claims;
