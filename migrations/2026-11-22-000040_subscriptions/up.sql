-- Subscription lifecycle tables for the Unified Pentagonal Project
-- (`src/modules/store.rs` + `src/modules/billing.rs`): the
-- commercial subscription created when a subscription order is
-- paid, its append-only lifecycle audit trail, and the recorded
-- failed renewal payments (dunning attempts).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - The store owns the authoritative subscription record
--     (`Subscription` in store.rs); billing keeps a mirror
--     (`SubscriptionLifecycle` in billing.rs) and drives lifecycle
--     decisions through `subscription.lifecycle.apply` events,
--     which the store applies. The `subscriptions` table is the
--     durable form of the store record.
--   - `subscriptions.status` is `trialing`, `active`, `past_due`,
--     or `cancelled`. A subscription cancelled at period end keeps
--     its status (`active` or `trialing`) with
--     `cancel_at_period_end` set until the period ends, so access
--     continues until then; `cancelled_at` is set only by an
--     immediate cancellation (or by the period-end/grace
--     cancellation when it finally applies).
--   - `subscriptions.price` is the recurring price per period in
--     major units as stored in memory (f64); the durable form is
--     NUMERIC(19,4). `pending_*` holds a plan change waiting for
--     the next period end (`no_prorate` and `credit_only`
--     changes); a `prorate` change applies immediately and never
--     lands in `pending_*`.
--   - `subscriptions.module_id` names a sellable module when the
--     product is a module offering: an active subscription
--     covering it counts as a module entitlement (see
--     `subscription.module_entitlement.check` in store.rs and the
--     licensing convergence note in
--     INTEGRATION_NOTES_BATCH3_B5.md).
--   - `subscriptions.product_id`, `tier_id`, `order_id`, and
--     `user_id` are plain references (VARCHAR) on purpose, matching
--     the store's string ids: the store catalogue and orders are
--     in-memory records with string ids, not Diesel rows, so no
--     foreign key can reference them. `company_id` is a UUID
--     reference with NO foreign key for the same reason (companies
--     are not a Diesel table in this migration set).
--   - `subscription_events` is append-only: one row per lifecycle
--     transition (`created`, `trial_ended`, `renewed`, `upgraded`,
--     `downgraded`, `payment_failed`, `past_due`, `cancelled`,
--     `entitlement_changed`). Rows are never updated or deleted by
--     the application; `payload` carries the event detail (amounts,
--     proration lines, reason) as JSONB.
--   - `dunning_attempts` records every failed renewal payment,
--     whether or not a retry follows. `attempt_number` is 1-based
--     within the current dunning run and restarts at 1 after a
--     successful renewal (a new period clears the run). The
--     `subscription_id` foreign key is ON DELETE CASCADE: deleting
--     a subscription deletes its dunning history, but the
--     `subscription_events` audit trail is kept (its
--     `subscription_id` is a plain UUID with NO foreign key on
--     purpose, so the audit survives the record it describes).
--   - Money is NUMERIC(19,4) in SQL. No DOUBLE PRECISION column
--     exists in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One paid subscription: the commercial record of a recurring
-- purchase, created when a subscription order is paid.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR NOT NULL,
    company_id UUID,
    product_id VARCHAR NOT NULL,
    tier_id VARCHAR,
    order_id VARCHAR NOT NULL,
    module_id VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('trialing', 'active', 'past_due', 'cancelled')),
    billing_period VARCHAR NOT NULL DEFAULT 'monthly'
        CHECK (billing_period IN ('weekly', 'monthly', 'yearly')),
    price NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (price >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (char_length(currency) = 3),
    trial_days INTEGER NOT NULL DEFAULT 0
        CHECK (trial_days >= 0 AND trial_days <= 3650),
    trial_end TIMESTAMPTZ,
    current_period_start TIMESTAMPTZ NOT NULL,
    current_period_end TIMESTAMPTZ NOT NULL,
    proration_policy VARCHAR NOT NULL DEFAULT 'prorate'
        CHECK (proration_policy IN ('prorate', 'no_prorate', 'credit_only')),
    grace_period_days INTEGER NOT NULL DEFAULT 0
        CHECK (grace_period_days >= 0 AND grace_period_days <= 90),
    dunning_max_retries INTEGER NOT NULL DEFAULT 3
        CHECK (dunning_max_retries >= 0 AND dunning_max_retries <= 10),
    dunning_retry_interval_days INTEGER NOT NULL DEFAULT 3
        CHECK (dunning_retry_interval_days >= 1 AND dunning_retry_interval_days <= 30),
    cancel_at_period_end BOOLEAN NOT NULL DEFAULT FALSE,
    cancelled_at TIMESTAMPTZ,
    pending_product_id VARCHAR,
    pending_tier_id VARCHAR,
    pending_price NUMERIC(19,4) CHECK (pending_price IS NULL OR pending_price >= 0),
    pending_billing_period VARCHAR
        CHECK (pending_billing_period IS NULL OR pending_billing_period IN ('weekly', 'monthly', 'yearly')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (current_period_end > current_period_start),
    CHECK (trial_end IS NULL OR trial_end >= current_period_start)
);

CREATE INDEX IF NOT EXISTS idx_subscriptions_user_id
    ON subscriptions (user_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_company_id
    ON subscriptions (company_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_product_id
    ON subscriptions (product_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_order_id
    ON subscriptions (order_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_module_id
    ON subscriptions (module_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_status
    ON subscriptions (status);
CREATE INDEX IF NOT EXISTS idx_subscriptions_current_period_end
    ON subscriptions (current_period_end);

-- ---------------------------------------------------------------------------
-- Append-only lifecycle audit trail for subscriptions. One row per
-- lifecycle transition; rows are never updated or deleted.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS subscription_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id UUID NOT NULL,
    event VARCHAR NOT NULL
        CHECK (event IN ('created', 'trial_ended', 'renewed', 'upgraded', 'downgraded', 'payment_failed', 'past_due', 'cancelled', 'entitlement_changed')),
    actor VARCHAR,
    reason VARCHAR,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_subscription_events_subscription_id
    ON subscription_events (subscription_id);
CREATE INDEX IF NOT EXISTS idx_subscription_events_event
    ON subscription_events (event);
CREATE INDEX IF NOT EXISTS idx_subscription_events_created_at
    ON subscription_events (created_at);

-- ---------------------------------------------------------------------------
-- One recorded failed renewal payment (dunning attempt).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS dunning_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id UUID NOT NULL REFERENCES subscriptions (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    attempt_number INTEGER NOT NULL
        CHECK (attempt_number >= 1),
    reason VARCHAR NOT NULL,
    amount_minor BIGINT CHECK (amount_minor IS NULL OR amount_minor >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_dunning_attempts_subscription_id
    ON dunning_attempts (subscription_id);
CREATE INDEX IF NOT EXISTS idx_dunning_attempts_created_at
    ON dunning_attempts (created_at);
