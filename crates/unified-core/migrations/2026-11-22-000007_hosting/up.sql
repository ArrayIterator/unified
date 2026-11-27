-- Hosting tables for the Unified Pentagonal Project (hosting.rs):
-- hosting plans (sellable resource bundles) and hosting accounts
-- (one plan bound to one domain and one company), plus the paid
-- order claims recorded from store events.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `hosting_plans.price` is NUMERIC(19,4): money is decimal,
--     never DOUBLE PRECISION (see migration 000003).
--   - `hosting_plans.store_product_id` is VARCHAR, not a foreign
--     key: the Store keeps string product ids (the same reservation
--     as the `store_orders.user_id` follow-up in docs/SCHEMA.md).
--   - `hosting_accounts.store_order_id` is VARCHAR for the same
--     reason; the order itself lives in `store_orders`.
--   - `hosting_accounts.domain` is lower-cased by the code and has
--     no foreign key to `domains`: an account may be created for a
--     domain that is only observed, not registered.
--   - `hosting_order_claims` records paid store orders for a plan
--     as provisioning claims; one claim per (order, plan).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- A hosting plan: a sellable resource bundle (disk / CPU / RAM /
-- bandwidth, price, billing cycle), optionally linked to a store
-- product id so the plan can be sold through the Store.
CREATE TABLE IF NOT EXISTS hosting_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    disk_gb BIGINT NOT NULL CHECK (disk_gb >= 0),
    cpu_cores DOUBLE PRECISION NOT NULL CHECK (cpu_cores >= 0),
    ram_mb BIGINT NOT NULL CHECK (ram_mb >= 0),
    bandwidth_gb BIGINT NOT NULL CHECK (bandwidth_gb >= 0),
    price NUMERIC(19, 4) NOT NULL CHECK (price >= 0),
    currency VARCHAR NOT NULL,
    billing_cycle VARCHAR NOT NULL CHECK (billing_cycle IN ('monthly', 'yearly')),
    store_product_id VARCHAR,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_hosting_plans_store_product_id
    ON hosting_plans (store_product_id);
CREATE INDEX IF NOT EXISTS idx_hosting_plans_is_active
    ON hosting_plans (is_active);

-- A hosting account: one plan bound to one domain and one company.
-- Lifecycle: pending -> active -> suspended -> active, with
-- cancelled as the terminal state.
CREATE TABLE IF NOT EXISTS hosting_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID NOT NULL REFERENCES hosting_plans (id) ON DELETE RESTRICT ON UPDATE NO ACTION,
    domain VARCHAR NOT NULL,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    status VARCHAR NOT NULL CHECK (status IN ('pending', 'active', 'suspended', 'cancelled')),
    ssl_state VARCHAR NOT NULL DEFAULT 'none' CHECK (ssl_state IN ('none', 'pending', 'active', 'expired', 'renewing')),
    disk_used_mb BIGINT NOT NULL DEFAULT 0 CHECK (disk_used_mb >= 0),
    bandwidth_used_mb BIGINT NOT NULL DEFAULT 0 CHECK (bandwidth_used_mb >= 0),
    store_order_id VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_hosting_accounts_plan_id
    ON hosting_accounts (plan_id);
CREATE INDEX IF NOT EXISTS idx_hosting_accounts_domain
    ON hosting_accounts (domain);
CREATE INDEX IF NOT EXISTS idx_hosting_accounts_company_id
    ON hosting_accounts (company_id);
CREATE INDEX IF NOT EXISTS idx_hosting_accounts_status
    ON hosting_accounts (status);
CREATE INDEX IF NOT EXISTS idx_hosting_accounts_store_order_id
    ON hosting_accounts (store_order_id);

-- A paid store order for a hosting plan, recorded from the
-- `store.order.completed` / `order.paid` hook events, so an account
-- created with `store_order_id` can be verified against a real
-- paid order.
CREATE TABLE IF NOT EXISTS hosting_order_claims (
    order_id VARCHAR NOT NULL,
    plan_id UUID NOT NULL REFERENCES hosting_plans (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id VARCHAR NOT NULL,
    company_id VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (order_id, plan_id)
);

CREATE INDEX IF NOT EXISTS idx_hosting_order_claims_plan_id
    ON hosting_order_claims (plan_id);
