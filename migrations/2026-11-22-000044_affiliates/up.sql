-- Affiliate / Referral tables for the Unified Pentagonal Project
-- (`src/modules/affiliates.rs`): affiliates with an approval
-- workflow, per-product commission rules, referral links and codes,
-- tracked clicks, signup / purchase conversions, commissions, and
-- payout requests (billing owns the actual payment).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/affiliates.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `affiliates.user_id` is a plain VARCHAR user id: the module
--     identifies callers by the `X-User-Id` header, exactly like
--     `store_orders.user_id` in migration 000002. One affiliate
--     record per user (UNIQUE on `user_id`). `affiliates.company_id`
--     is likewise a plain VARCHAR reference to the company module.
--   - `affiliates.status` is `pending`, `approved`, `rejected`, or
--     `suspended`. `commission_type` is `percent` (value 0..100)
--     or `fixed` (a flat amount); money is NUMERIC(19,4).
--     `attribution_window_days` is 1..365 (default 30, last-click
--     wins inside the window).
--   - `affiliate_commission_rules` holds one per-product override
--     per affiliate (UNIQUE on affiliate + product); the affiliate
--     default applies when no rule matches a product.
--   - `affiliate_links.code` is globally unique (the value
--     visitors arrive with). `campaign` groups links; `product_id`
--     narrows a link to one product.
--   - `affiliate_clicks.ip_hash` is a one-way SHA-256 hex digest
--     of the visitor IP; the raw IP is never stored. `flagged`
--     with `flag_reasons` is advisory only: a flagged click is
--     still counted and flagging never blocks, bans, or voids
--     anything on its own.
--   - `affiliate_conversions.kind` is `signup` or `purchase`.
--     The partial unique index on `order_id` allows at most one
--     purchase conversion per order, so a duplicate
--     `store.order.completed` emit can never earn a second
--     commission. `flagged` marks conversions needing human review
--     (for example self-referral).
--   - `affiliate_commissions.status` is `pending`, `approved`,
--     `paid`, or `void`. `payout_id` is a plain UUID reference to
--     `affiliate_payouts`, not a foreign key: the payout owns the
--     commission list (`commission_ids`), so the back-reference
--     stays a plain id to avoid a circular foreign key.
--   - `affiliate_payouts.commission_ids` is the UUID array of the
--     approved commissions paid together. This module only
--     requests payouts; the billing module moves the money and the
--     payout is marked paid here afterwards.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One affiliate (partner).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS affiliates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR(128) NOT NULL UNIQUE,
    display_name VARCHAR(200) NOT NULL,
    email VARCHAR(254) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'suspended')),
    commission_type VARCHAR(16) NOT NULL DEFAULT 'percent'
        CHECK (commission_type IN ('percent', 'fixed')),
    commission_value NUMERIC(19,4) NOT NULL DEFAULT 10.0000
        CHECK (commission_value >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    attribution_window_days INTEGER NOT NULL DEFAULT 30
        CHECK (attribution_window_days BETWEEN 1 AND 365),
    company_id VARCHAR(128),
    payout_method VARCHAR(64),
    payout_reference VARCHAR(200),
    reviewed_by VARCHAR(128),
    review_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_affiliates_status ON affiliates (status);
CREATE INDEX IF NOT EXISTS idx_affiliates_company_id ON affiliates (company_id);

-- ---------------------------------------------------------------------------
-- Per-product commission override for one affiliate.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS affiliate_commission_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    affiliate_id UUID NOT NULL REFERENCES affiliates (id) ON DELETE CASCADE,
    product_id VARCHAR(128) NOT NULL,
    commission_type VARCHAR(16) NOT NULL
        CHECK (commission_type IN ('percent', 'fixed')),
    commission_value NUMERIC(19,4) NOT NULL CHECK (commission_value >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (affiliate_id, product_id)
);

CREATE INDEX IF NOT EXISTS idx_affiliate_commission_rules_affiliate_id
    ON affiliate_commission_rules (affiliate_id);

-- ---------------------------------------------------------------------------
-- One referral link / code.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS affiliate_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    affiliate_id UUID NOT NULL REFERENCES affiliates (id) ON DELETE CASCADE,
    code VARCHAR(64) NOT NULL UNIQUE,
    campaign VARCHAR(128),
    product_id VARCHAR(128),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_affiliate_links_affiliate_id
    ON affiliate_links (affiliate_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_links_code ON affiliate_links (code);

-- ---------------------------------------------------------------------------
-- One recorded click on a referral link.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS affiliate_clicks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    link_id UUID NOT NULL REFERENCES affiliate_links (id) ON DELETE CASCADE,
    affiliate_id UUID NOT NULL REFERENCES affiliates (id) ON DELETE CASCADE,
    ip_hash VARCHAR(64) NOT NULL,
    user_agent VARCHAR(512),
    referrer VARCHAR(2048),
    flagged BOOLEAN NOT NULL DEFAULT FALSE,
    flag_reasons TEXT[] NOT NULL DEFAULT '{}',
    converted BOOLEAN NOT NULL DEFAULT FALSE,
    clicked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_affiliate_clicks_link_id ON affiliate_clicks (link_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_clicks_affiliate_id ON affiliate_clicks (affiliate_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_clicks_ip_hash ON affiliate_clicks (ip_hash);
CREATE INDEX IF NOT EXISTS idx_affiliate_clicks_clicked_at ON affiliate_clicks (clicked_at);

-- ---------------------------------------------------------------------------
-- One tracked conversion (signup or purchase).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS affiliate_conversions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    affiliate_id UUID NOT NULL REFERENCES affiliates (id) ON DELETE CASCADE,
    link_id UUID REFERENCES affiliate_links (id) ON DELETE SET NULL,
    click_id UUID REFERENCES affiliate_clicks (id) ON DELETE SET NULL,
    customer_user_id VARCHAR(128) NOT NULL,
    kind VARCHAR(16) NOT NULL CHECK (kind IN ('signup', 'purchase')),
    order_id VARCHAR(200),
    order_total NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (order_total >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    product_id VARCHAR(128),
    flagged BOOLEAN NOT NULL DEFAULT FALSE,
    flag_reasons TEXT[] NOT NULL DEFAULT '{}',
    attributed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_affiliate_conversions_affiliate_id
    ON affiliate_conversions (affiliate_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_conversions_order_id
    ON affiliate_conversions (order_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_conversions_kind
    ON affiliate_conversions (kind);
-- At most one purchase conversion per order.
CREATE UNIQUE INDEX IF NOT EXISTS uq_affiliate_conversions_purchase_order
    ON affiliate_conversions (order_id)
    WHERE kind = 'purchase' AND order_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- One commission earned from a purchase conversion.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS affiliate_commissions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    affiliate_id UUID NOT NULL REFERENCES affiliates (id) ON DELETE CASCADE,
    conversion_id UUID NOT NULL REFERENCES affiliate_conversions (id) ON DELETE CASCADE,
    order_id VARCHAR(200),
    product_id VARCHAR(128),
    commission_type VARCHAR(16) NOT NULL
        CHECK (commission_type IN ('percent', 'fixed')),
    rate_or_amount NUMERIC(19,4) NOT NULL CHECK (rate_or_amount >= 0),
    amount NUMERIC(19,4) NOT NULL CHECK (amount >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'paid', 'void')),
    payout_id UUID,
    flagged BOOLEAN NOT NULL DEFAULT FALSE,
    fraud_flags TEXT[] NOT NULL DEFAULT '{}',
    reviewed_by VARCHAR(128),
    review_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_affiliate_commissions_affiliate_id
    ON affiliate_commissions (affiliate_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_commissions_status
    ON affiliate_commissions (status);
CREATE INDEX IF NOT EXISTS idx_affiliate_commissions_conversion_id
    ON affiliate_commissions (conversion_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_commissions_order_id
    ON affiliate_commissions (order_id);

-- ---------------------------------------------------------------------------
-- One payout request: approved commissions paid out together.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS affiliate_payouts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    affiliate_id UUID NOT NULL REFERENCES affiliates (id) ON DELETE CASCADE,
    commission_ids UUID[] NOT NULL DEFAULT '{}',
    total_amount NUMERIC(19,4) NOT NULL CHECK (total_amount >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR(16) NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'approved', 'paid', 'rejected')),
    method VARCHAR(64),
    reference VARCHAR(200),
    requested_by VARCHAR(128) NOT NULL,
    reviewed_by VARCHAR(128),
    review_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_affiliate_payouts_affiliate_id
    ON affiliate_payouts (affiliate_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_payouts_status ON affiliate_payouts (status);
