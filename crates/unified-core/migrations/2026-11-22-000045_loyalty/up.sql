-- Loyalty / Rewards tables for the Unified Pentagonal Project
-- (`src/modules/loyalty.rs`): points programs, earn rules, tiers,
-- the points ledger, and redemptions.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/loyalty.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `loyalty_programs.company_id` is a plain VARCHAR reference
--     to the company module (NULL = the default program, used
--     when an order or signup carries no company). At most one
--     program is active per company: the partial unique index on
--     `COALESCE(company_id, '')` enforces it, including the
--     default program. `expiry_days` is NULL when points never
--     expire; `expiry_mode` is `fixed` (each earn expires after
--     `expiry_days`) or `inactivity` (the whole balance expires
--     after `expiry_days` without ledger activity).
--     `discount_per_point` converts points to currency on
--     redemption (NUMERIC(19,4)).
--   - `loyalty_earn_rules.kind` is `purchase`, `referral`,
--     `review`, or `signup`. `points_per_unit` is points per one
--     unit of currency for purchase rules; `fixed_points` is the
--     flat award for the other kinds. `cap_per_day` bounds earns
--     of that kind per customer per UTC calendar day, `cap_total`
--     over all time. The partial unique index allows at most one
--     active rule per (program, kind).
--   - `loyalty_tiers` qualify a customer when lifetime earned
--     points reach `min_lifetime_points` AND lifetime spend
--     reaches `min_lifetime_spend`; the highest qualifying tier
--     (by `sort_order`, then thresholds) is the customer's tier.
--     `benefits` are free-form benefit descriptions.
--   - `loyalty_ledger` is the source of truth for balances:
--     EVERY points movement is a row (`earn`, `spend`,
--     `expire`, `adjust`) with a signed `points` delta, the
--     reason, the actor, and `balance_after`. Balances are always
--     computed by summing these rows, never stored as a bare
--     number. `earn_kind` records which earn rule kind produced
--     an earn row; `reference_id` is the order / referral id;
--     `reference_amount` is the order total for purchase earns
--     (lifetime spend). `expires_at` is set on earn rows under
--     fixed expiry. Expiry is a ledger row, never a delete.
--     Staff recognition points live in the gamification module
--     with a SEPARATE ledger; nothing here mixes with them.
--   - `loyalty_redemptions.kind` is `discount` (points to a
--     discount on an order; `discount_amount` is computed from
--     the program's `discount_per_point`) or `product` (points
--     to a product). A cancelled redemption refunds its points
--     through a new ledger row; the original spend row is never
--     rewritten.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One points program.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS loyalty_programs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR(128),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    points_name VARCHAR(64) NOT NULL DEFAULT 'points',
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    discount_per_point NUMERIC(19,4) NOT NULL DEFAULT 0.0100
        CHECK (discount_per_point >= 0),
    active BOOLEAN NOT NULL DEFAULT FALSE,
    expiry_days INTEGER CHECK (expiry_days >= 1),
    expiry_mode VARCHAR(16) NOT NULL DEFAULT 'fixed'
        CHECK (expiry_mode IN ('fixed', 'inactivity')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_loyalty_programs_company_id
    ON loyalty_programs (company_id);
CREATE INDEX IF NOT EXISTS idx_loyalty_programs_active ON loyalty_programs (active);
-- One active program at a time per company (the default program,
-- with NULL company_id, counts as its own group).
CREATE UNIQUE INDEX IF NOT EXISTS uq_loyalty_programs_one_active_per_company
    ON loyalty_programs (COALESCE(company_id, ''))
    WHERE active;

-- ---------------------------------------------------------------------------
-- One earn rule of a program.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS loyalty_earn_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id UUID NOT NULL REFERENCES loyalty_programs (id) ON DELETE CASCADE,
    kind VARCHAR(16) NOT NULL
        CHECK (kind IN ('purchase', 'referral', 'review', 'signup')),
    points_per_unit NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (points_per_unit >= 0),
    fixed_points BIGINT CHECK (fixed_points >= 0),
    cap_per_day BIGINT CHECK (cap_per_day >= 1),
    cap_total BIGINT CHECK (cap_total >= 1),
    min_order_amount NUMERIC(19,4) CHECK (min_order_amount >= 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_loyalty_earn_rules_program_id
    ON loyalty_earn_rules (program_id);
-- At most one active rule per (program, kind).
CREATE UNIQUE INDEX IF NOT EXISTS uq_loyalty_earn_rules_one_active_per_kind
    ON loyalty_earn_rules (program_id, kind)
    WHERE active;

-- ---------------------------------------------------------------------------
-- One tier of a program.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS loyalty_tiers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id UUID NOT NULL REFERENCES loyalty_programs (id) ON DELETE CASCADE,
    name VARCHAR(200) NOT NULL,
    min_lifetime_points BIGINT NOT NULL DEFAULT 0 CHECK (min_lifetime_points >= 0),
    min_lifetime_spend NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (min_lifetime_spend >= 0),
    benefits TEXT[] NOT NULL DEFAULT '{}',
    discount_percent NUMERIC(5,2) NOT NULL DEFAULT 0
        CHECK (discount_percent >= 0 AND discount_percent <= 100),
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_loyalty_tiers_program_id ON loyalty_tiers (program_id);

-- ---------------------------------------------------------------------------
-- One ledger row: every points movement.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS loyalty_ledger (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id UUID NOT NULL REFERENCES loyalty_programs (id) ON DELETE CASCADE,
    customer_id VARCHAR(128) NOT NULL,
    kind VARCHAR(16) NOT NULL
        CHECK (kind IN ('earn', 'spend', 'expire', 'adjust')),
    points BIGINT NOT NULL,
    balance_after BIGINT NOT NULL CHECK (balance_after >= 0),
    reason TEXT NOT NULL,
    actor VARCHAR(128) NOT NULL,
    earn_kind VARCHAR(16)
        CHECK (earn_kind IS NULL OR earn_kind IN ('purchase', 'referral', 'review', 'signup')),
    reference_id VARCHAR(200),
    reference_amount NUMERIC(19,4) CHECK (reference_amount >= 0),
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_loyalty_ledger_program_id ON loyalty_ledger (program_id);
CREATE INDEX IF NOT EXISTS idx_loyalty_ledger_customer_id ON loyalty_ledger (customer_id);
CREATE INDEX IF NOT EXISTS idx_loyalty_ledger_program_customer
    ON loyalty_ledger (program_id, customer_id);
CREATE INDEX IF NOT EXISTS idx_loyalty_ledger_kind ON loyalty_ledger (kind);
CREATE INDEX IF NOT EXISTS idx_loyalty_ledger_created_at ON loyalty_ledger (created_at);

-- ---------------------------------------------------------------------------
-- One redemption: points spent for a discount or a product.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS loyalty_redemptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id UUID NOT NULL REFERENCES loyalty_programs (id) ON DELETE CASCADE,
    customer_id VARCHAR(128) NOT NULL,
    points_spent BIGINT NOT NULL CHECK (points_spent > 0),
    kind VARCHAR(16) NOT NULL CHECK (kind IN ('discount', 'product')),
    discount_amount NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (discount_amount >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    product_id VARCHAR(128),
    order_id VARCHAR(200),
    status VARCHAR(16) NOT NULL DEFAULT 'completed'
        CHECK (status IN ('completed', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_loyalty_redemptions_program_id
    ON loyalty_redemptions (program_id);
CREATE INDEX IF NOT EXISTS idx_loyalty_redemptions_customer_id
    ON loyalty_redemptions (customer_id);
CREATE INDEX IF NOT EXISTS idx_loyalty_redemptions_status
    ON loyalty_redemptions (status);
