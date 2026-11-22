-- Pricing Tables + Marketplace Builder tables for the Unified Pentagonal Project
-- (`src/modules/pricing_tables.rs`): reusable pricing tables with tiers and
-- feature rows, plus marketplace vendors, commission rules, product
-- approvals, the computed commission ledger, and vendor payouts.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - Money is NUMERIC(19,4) on every money column
--     (`pricing_tiers.monthly_price` / `yearly_price`,
--     `marketplace_commissions.order_total` /
--     `commission_amount`, `marketplace_payouts.amount`).
--     Rates and discounts are NUMERIC(5,2) CHECK 0..=100.
--   - `pricing_tiers.highlighted` carries no database-level
--     exactly-one constraint: the module enforces exactly one
--     highlighted tier per table on every write (first tier is
--     highlighted, highlighting clears the others, clearing the
--     only highlight is rejected, deleting the highlighted tier
--     promotes the next one). A partial unique index cannot express
--     "exactly one true per table while tiers exist", so the rule
--     lives in code and is documented in pricing_tables.rs.
--   - `marketplace_vendors.user_id` is UNIQUE: one vendor record
--     per user. `company_id` is a plain VARCHAR reference (no
--     foreign key): the vendor may sell for a company record owned
--     by another module, and deleting that company must not delete
--     the vendor.
--   - `marketplace_product_approvals.product_id` is UNIQUE: one
--     approval record per product. The Store module is the source
--     of truth for the product itself, so there is deliberately NO
--     foreign key to any product table; this table stores only the
--     approval state, the vendor link, and the category captured at
--     submission time.
--   - `marketplace_commissions` is the computed ledger, written
--     exactly once per (order, product) pair by the paid-order hook
--     handlers; the UNIQUE (order_id, product_id) index is the
--     database side of that idempotency. `order_id` and
--     `product_id` are plain VARCHAR references (no foreign keys):
--     orders and products live in the Store module.
--   - `marketplace_payouts.invoice_id` is filled when Billing emits
--     `invoice.created` for a payout; it is a plain VARCHAR
--     reference, not a foreign key.
--   - `marketplace_commission_rules` has one rule per
--     (scope, scope_value) pair, enforced by the unique index on
--     (scope, COALESCE(scope_value, '')) so the single global rule
--     (empty scope_value) is covered too. `scope` is `vendor`,
--     `category`, or `global`; `scope_value` is the vendor id, the
--     category slug, or empty for a global rule.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One reusable pricing table.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS pricing_tables (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    slug VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    layout VARCHAR NOT NULL DEFAULT 'grid'
        CHECK (layout IN ('grid', 'columns', 'comparison')),
    theme_accent VARCHAR NOT NULL DEFAULT '#0B6B3A'
        CHECK (theme_accent ~ '^#[0-9a-fA-F]{6}$'),
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published')),
    yearly_discount_percent NUMERIC(5,2) NOT NULL DEFAULT 0
        CHECK (yearly_discount_percent >= 0 AND yearly_discount_percent <= 100),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_pricing_tables_slug
    ON pricing_tables (slug);
CREATE INDEX IF NOT EXISTS idx_pricing_tables_status
    ON pricing_tables (status);
CREATE INDEX IF NOT EXISTS idx_pricing_tables_currency
    ON pricing_tables (currency);

-- ---------------------------------------------------------------------------
-- One tier of a pricing table.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS pricing_tiers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    table_id UUID NOT NULL REFERENCES pricing_tables (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    monthly_price NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (monthly_price >= 0),
    yearly_price NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (yearly_price >= 0),
    cta_label VARCHAR NOT NULL DEFAULT 'Get started',
    cta_url VARCHAR NOT NULL DEFAULT '/pricing',
    highlighted BOOLEAN NOT NULL DEFAULT FALSE,
    sort_order INTEGER NOT NULL DEFAULT 0 CHECK (sort_order >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pricing_tiers_table_id
    ON pricing_tiers (table_id);
CREATE INDEX IF NOT EXISTS idx_pricing_tiers_table_sort
    ON pricing_tiers (table_id, sort_order);
CREATE INDEX IF NOT EXISTS idx_pricing_tiers_highlighted
    ON pricing_tiers (table_id, highlighted);

-- ---------------------------------------------------------------------------
-- One feature row of a tier.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS pricing_tier_features (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tier_id UUID NOT NULL REFERENCES pricing_tiers (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    feature_text VARCHAR NOT NULL,
    included BOOLEAN NOT NULL DEFAULT TRUE,
    note TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0 CHECK (sort_order >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pricing_tier_features_tier_id
    ON pricing_tier_features (tier_id);
CREATE INDEX IF NOT EXISTS idx_pricing_tier_features_tier_sort
    ON pricing_tier_features (tier_id, sort_order);

-- ---------------------------------------------------------------------------
-- One marketplace vendor.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS marketplace_vendors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR NOT NULL,
    company_id VARCHAR,
    company_name VARCHAR NOT NULL,
    slug VARCHAR NOT NULL,
    email VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'suspended', 'rejected')),
    application_notes TEXT NOT NULL DEFAULT '',
    review_reason TEXT NOT NULL DEFAULT '',
    reviewed_by VARCHAR,
    reviewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_marketplace_vendors_user_id
    ON marketplace_vendors (user_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_marketplace_vendors_slug
    ON marketplace_vendors (slug);
CREATE INDEX IF NOT EXISTS idx_marketplace_vendors_status
    ON marketplace_vendors (status);
CREATE INDEX IF NOT EXISTS idx_marketplace_vendors_company_id
    ON marketplace_vendors (company_id);

-- ---------------------------------------------------------------------------
-- One commission rule: per-vendor, per-category, or global.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS marketplace_commission_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope VARCHAR NOT NULL
        CHECK (scope IN ('vendor', 'category', 'global')),
    scope_value VARCHAR NOT NULL DEFAULT '',
    rate_percent NUMERIC(5,2) NOT NULL
        CHECK (rate_percent >= 0 AND rate_percent <= 100),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_marketplace_commission_rules_scope
    ON marketplace_commission_rules (scope, COALESCE(scope_value, ''));
CREATE INDEX IF NOT EXISTS idx_marketplace_commission_rules_scope
    ON marketplace_commission_rules (scope);

-- ---------------------------------------------------------------------------
-- Approval state for one vendor product. The Store module owns the
-- product; this table holds only the approval state + vendor link.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS marketplace_product_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id VARCHAR NOT NULL,
    vendor_id UUID NOT NULL REFERENCES marketplace_vendors (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    category VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    reason TEXT NOT NULL DEFAULT '',
    reviewed_by VARCHAR,
    reviewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_marketplace_product_approvals_product_id
    ON marketplace_product_approvals (product_id);
CREATE INDEX IF NOT EXISTS idx_marketplace_product_approvals_vendor_id
    ON marketplace_product_approvals (vendor_id);
CREATE INDEX IF NOT EXISTS idx_marketplace_product_approvals_status
    ON marketplace_product_approvals (status);

-- ---------------------------------------------------------------------------
-- One computed commission ledger entry: exactly one per
-- (order, product) pair.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS marketplace_commissions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id VARCHAR NOT NULL,
    vendor_id UUID NOT NULL REFERENCES marketplace_vendors (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    product_id VARCHAR NOT NULL,
    order_total NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (order_total >= 0),
    rate_percent NUMERIC(5,2) NOT NULL
        CHECK (rate_percent >= 0 AND rate_percent <= 100),
    commission_amount NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (commission_amount >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'paid', 'reversed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_marketplace_commissions_order_product
    ON marketplace_commissions (order_id, product_id);
CREATE INDEX IF NOT EXISTS idx_marketplace_commissions_vendor_id
    ON marketplace_commissions (vendor_id);
CREATE INDEX IF NOT EXISTS idx_marketplace_commissions_order_id
    ON marketplace_commissions (order_id);
CREATE INDEX IF NOT EXISTS idx_marketplace_commissions_status
    ON marketplace_commissions (status);

-- ---------------------------------------------------------------------------
-- One vendor payout request.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS marketplace_payouts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vendor_id UUID NOT NULL REFERENCES marketplace_vendors (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    amount NUMERIC(19,4) NOT NULL
        CHECK (amount > 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    status VARCHAR NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'approved', 'paid', 'rejected', 'cancelled')),
    invoice_id VARCHAR,
    reference VARCHAR NOT NULL DEFAULT '',
    requested_by VARCHAR NOT NULL,
    reviewed_by VARCHAR,
    review_reason TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_marketplace_payouts_vendor_id
    ON marketplace_payouts (vendor_id);
CREATE INDEX IF NOT EXISTS idx_marketplace_payouts_status
    ON marketplace_payouts (status);
CREATE INDEX IF NOT EXISTS idx_marketplace_payouts_invoice_id
    ON marketplace_payouts (invoice_id);
