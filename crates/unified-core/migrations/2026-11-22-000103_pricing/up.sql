-- Smart Pricing tables for the Unified Pentagonal Project
-- (`src/modules/pricing.rs`): price recommendations (cost-plus,
-- tiered, bundle), discount rules, coupons and redemptions, stored
-- tiers and bundles, human-entered market reference prices,
-- versioned prices with second-approver records, proration
-- records, credit notes, reconciliation runs, per-tenant settings,
-- and the currency registry. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the saved-views precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All money amounts are NUMERIC(19,4). In memory the module
--     keeps integer scaled minor units (1 major unit = 10,000
--     scaled units), which map exactly onto NUMERIC(19,4); the
--     NUMERIC columns are the durable form of those amounts.
--     Percentages and rates are integer basis points (10,000 =
--     100%) stored as BIGINT, never as floats.
--   - Tenant scoping (standing rule): every tenant-business table
--     carries `tenant_id` UUID NOT NULL (the tenant id is a uuidv7
--     from the tenant context). No tenant context means fail
--     closed; nothing falls back to the `public` schema. The
--     tables below are created in the schema the migration runner
--     targets today; when batch 17 lands the schema-per-tenant
--     plumbing, the integrator re-points these tables at the
--     per-tenant schema - every table already carries `tenant_id`,
--     so no column changes are needed.
--   - `pricing_currencies` is the one exception: ISO 4217 codes
--     are standard constants and the currency rows are platform
--     reference data, identical for every tenant, so the table has
--     no `tenant_id`. The currency endpoints still require a
--     resolved tenant context.
--   - `product_id`, `subscription_id`, `customer_id`,
--     `invoice_id`, and `recommendation_id` are plain TEXT/VARCHAR
--     with NO foreign key on purpose: products live in the
--     products/store modules, subscriptions in the store, and
--     invoices in billing, and this module must not assume their
--     table or migration order.
--   - `pricing_recommendations.inputs` and
--     `pricing_recommendations.lines` are JSONB: the inputs that
--     produced the recommendation and the full line-by-line
--     calculation, kept for audit.
--   - `pricing_price_versions.change_bp` is NULL when the previous
--     price was zero and the new price is above zero (an
--     unmeasurable change, which always needs a second approver).
--   - Tier overlap is enforced in code, not by a constraint: a
--     range-overlap exclusion needs a range type the module does
--     not use, and the open-ended tier (NULL max) has no clean
--     exclusion form. The create-tier endpoint rejects overlaps.
--   - Exactly one highlighted/active price per product is enforced
--     in code (activation supersedes the previous active version);
--     the UNIQUE (tenant_id, product_id, version) index only
--     guarantees version numbers never repeat.
--   - No secret is stored in any table here.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Per-tenant pricing settings: one row per tenant.
CREATE TABLE IF NOT EXISTS pricing_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    rounding_mode VARCHAR NOT NULL DEFAULT 'half_up'
        CHECK (rounding_mode IN ('half_up')),
    proration_basis VARCHAR NOT NULL DEFAULT 'per_day'
        CHECK (proration_basis IN ('per_day', 'per_month')),
    days_per_month BIGINT NOT NULL DEFAULT 30
        CHECK (days_per_month >= 1 AND days_per_month <= 366),
    approval_threshold_bp BIGINT NOT NULL DEFAULT 1000
        CHECK (approval_threshold_bp >= 0 AND approval_threshold_bp <= 10000),
    require_second_approver BOOLEAN NOT NULL DEFAULT TRUE,
    default_currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (default_currency ~ '^[A-Z]{3}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_pricing_settings_tenant
    ON pricing_settings (tenant_id);

-- Currency registry: platform reference data (ISO 4217 codes are the
-- sanctioned standard constants). Exchange rates are integer ratios
-- relative to the default currency, entered by an admin only.
CREATE TABLE IF NOT EXISTS pricing_currencies (
    code VARCHAR PRIMARY KEY CHECK (code ~ '^[A-Z]{3}$'),
    exponent BIGINT NOT NULL DEFAULT 2 CHECK (exponent IN (0, 2)),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    rate_numerator BIGINT CHECK (rate_numerator IS NULL OR rate_numerator > 0),
    rate_denominator BIGINT CHECK (rate_denominator IS NULL OR rate_denominator > 0),
    rate_fetched_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO pricing_currencies (code, exponent, active, rate_numerator, rate_denominator)
VALUES
    ('USD', 2, TRUE, 1, 1),
    ('EUR', 2, TRUE, NULL, NULL),
    ('GBP', 2, TRUE, NULL, NULL),
    ('AUD', 2, TRUE, NULL, NULL),
    ('CAD', 2, TRUE, NULL, NULL),
    ('CHF', 2, TRUE, NULL, NULL),
    ('CNY', 2, TRUE, NULL, NULL),
    ('IDR', 2, TRUE, NULL, NULL),
    ('SGD', 2, TRUE, NULL, NULL),
    ('JPY', 0, TRUE, NULL, NULL),
    ('KRW', 0, TRUE, NULL, NULL),
    ('VND', 0, TRUE, NULL, NULL)
ON CONFLICT (code) DO NOTHING;

-- Discount rules: kind, order, scope, and limits are data.
CREATE TABLE IF NOT EXISTS pricing_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('percent', 'fixed', 'buy_x_get_y', 'volume')),
    percent_bp BIGINT NOT NULL DEFAULT 0
        CHECK (percent_bp >= 0 AND percent_bp <= 10000),
    amount_scaled NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (amount_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    buy_quantity BIGINT NOT NULL DEFAULT 0 CHECK (buy_quantity >= 0),
    get_quantity BIGINT NOT NULL DEFAULT 0 CHECK (get_quantity >= 0),
    min_quantity BIGINT NOT NULL DEFAULT 0 CHECK (min_quantity >= 0),
    min_subtotal_scaled NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (min_subtotal_scaled >= 0),
    product_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    evaluation_order BIGINT NOT NULL DEFAULT 0,
    stackable BOOLEAN NOT NULL DEFAULT TRUE,
    exclusive BOOLEAN NOT NULL DEFAULT FALSE,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'draft', 'archived')),
    valid_from TIMESTAMPTZ,
    valid_to TIMESTAMPTZ,
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (valid_to IS NULL OR valid_from IS NULL OR valid_to > valid_from)
);

CREATE INDEX IF NOT EXISTS idx_pricing_rules_tenant
    ON pricing_rules (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_rules_tenant_order
    ON pricing_rules (tenant_id, evaluation_order);
CREATE INDEX IF NOT EXISTS idx_pricing_rules_status
    ON pricing_rules (tenant_id, status);

-- Coupons: a discount rule with a code, a validity window, a global
-- usage limit, and a per-customer usage limit.
CREATE TABLE IF NOT EXISTS pricing_coupons (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    code VARCHAR NOT NULL,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('percent', 'fixed', 'buy_x_get_y')),
    percent_bp BIGINT NOT NULL DEFAULT 0
        CHECK (percent_bp >= 0 AND percent_bp <= 10000),
    amount_scaled NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (amount_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    buy_quantity BIGINT NOT NULL DEFAULT 0 CHECK (buy_quantity >= 0),
    get_quantity BIGINT NOT NULL DEFAULT 0 CHECK (get_quantity >= 0),
    product_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    valid_from TIMESTAMPTZ,
    valid_to TIMESTAMPTZ,
    usage_limit BIGINT CHECK (usage_limit IS NULL OR usage_limit > 0),
    per_customer_limit BIGINT
        CHECK (per_customer_limit IS NULL OR per_customer_limit > 0),
    used_count BIGINT NOT NULL DEFAULT 0 CHECK (used_count >= 0),
    stackable BOOLEAN NOT NULL DEFAULT FALSE,
    exclusive BOOLEAN NOT NULL DEFAULT FALSE,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (valid_to IS NULL OR valid_from IS NULL OR valid_to > valid_from)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_pricing_coupons_tenant_code
    ON pricing_coupons (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_pricing_coupons_tenant
    ON pricing_coupons (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_coupons_active
    ON pricing_coupons (tenant_id, active);

-- Per-customer coupon redemption counts.
CREATE TABLE IF NOT EXISTS pricing_coupon_redemptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    coupon_id UUID NOT NULL REFERENCES pricing_coupons (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    customer_id VARCHAR NOT NULL,
    count BIGINT NOT NULL DEFAULT 0 CHECK (count >= 0),
    last_redeemed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_pricing_coupon_redemptions
    ON pricing_coupon_redemptions (tenant_id, coupon_id, customer_id);
CREATE INDEX IF NOT EXISTS idx_pricing_coupon_redemptions_coupon
    ON pricing_coupon_redemptions (coupon_id);

-- Price recommendations: suggestions only, with the full calculation.
CREATE TABLE IF NOT EXISTS pricing_recommendations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    product_id VARCHAR NOT NULL,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('cost_plus', 'tiered', 'bundle')),
    suggested_price_scaled NUMERIC(19,4) NOT NULL
        CHECK (suggested_price_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    lines JSONB NOT NULL DEFAULT '[]'::jsonb,
    inputs JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR NOT NULL DEFAULT 'suggested'
        CHECK (status IN ('suggested', 'confirmed', 'discarded')),
    created_by VARCHAR NOT NULL,
    confirmed_by VARCHAR,
    price_version_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pricing_recommendations_tenant
    ON pricing_recommendations (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_recommendations_product
    ON pricing_recommendations (tenant_id, product_id);
CREATE INDEX IF NOT EXISTS idx_pricing_recommendations_status
    ON pricing_recommendations (tenant_id, status);

-- Cost lines of a recommendation (durable form of the cost inputs).
CREATE TABLE IF NOT EXISTS pricing_cost_lines (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    recommendation_id UUID NOT NULL REFERENCES pricing_recommendations (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    label VARCHAR NOT NULL,
    source VARCHAR NOT NULL,
    cost_scaled NUMERIC(19,4) NOT NULL CHECK (cost_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$')
);

CREATE INDEX IF NOT EXISTS idx_pricing_cost_lines_recommendation
    ON pricing_cost_lines (recommendation_id);

-- Stored tiers of a tiered price definition.
CREATE TABLE IF NOT EXISTS pricing_volume_tiers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    product_id VARCHAR NOT NULL,
    min_quantity BIGINT NOT NULL CHECK (min_quantity >= 1),
    max_quantity BIGINT CHECK (max_quantity IS NULL OR max_quantity >= 1),
    unit_price_scaled NUMERIC(19,4) NOT NULL
        CHECK (unit_price_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (max_quantity IS NULL OR max_quantity >= min_quantity)
);

CREATE INDEX IF NOT EXISTS idx_pricing_volume_tiers_tenant_product
    ON pricing_volume_tiers (tenant_id, product_id, min_quantity);

-- Bundle definitions.
CREATE TABLE IF NOT EXISTS pricing_bundles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    name VARCHAR NOT NULL,
    product_id VARCHAR NOT NULL,
    discount_kind VARCHAR NOT NULL CHECK (discount_kind IN ('percent', 'fixed')),
    discount_percent_bp BIGINT NOT NULL DEFAULT 0
        CHECK (discount_percent_bp >= 0 AND discount_percent_bp <= 10000),
    discount_amount_scaled NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (discount_amount_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pricing_bundles_tenant
    ON pricing_bundles (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_bundles_product
    ON pricing_bundles (tenant_id, product_id);

-- Components of a stored bundle.
CREATE TABLE IF NOT EXISTS pricing_bundle_components (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    bundle_id UUID NOT NULL REFERENCES pricing_bundles (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    product_id VARCHAR NOT NULL,
    label VARCHAR NOT NULL DEFAULT '',
    price_scaled NUMERIC(19,4) NOT NULL CHECK (price_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$')
);

CREATE INDEX IF NOT EXISTS idx_pricing_bundle_components_bundle
    ON pricing_bundle_components (bundle_id);

-- Human-entered market reference prices. The source and the
-- observation date are mandatory: a reference without provenance
-- is rejected. References are never scraped.
CREATE TABLE IF NOT EXISTS pricing_reference_prices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    product_id VARCHAR NOT NULL,
    source VARCHAR NOT NULL,
    source_url VARCHAR,
    price_scaled NUMERIC(19,4) NOT NULL CHECK (price_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    observed_at TIMESTAMPTZ NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    entered_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pricing_reference_prices_tenant
    ON pricing_reference_prices (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_reference_prices_product
    ON pricing_reference_prices (tenant_id, product_id);
CREATE INDEX IF NOT EXISTS idx_pricing_reference_prices_observed
    ON pricing_reference_prices (tenant_id, product_id, observed_at);

-- Versioned prices with the approval threshold state.
CREATE TABLE IF NOT EXISTS pricing_price_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    product_id VARCHAR NOT NULL,
    version BIGINT NOT NULL CHECK (version >= 1),
    price_scaled NUMERIC(19,4) NOT NULL CHECK (price_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    previous_price_scaled NUMERIC(19,4) NOT NULL DEFAULT 0
        CHECK (previous_price_scaled >= 0),
    change_bp BIGINT CHECK (change_bp IS NULL OR change_bp >= 0),
    above_threshold BOOLEAN NOT NULL DEFAULT FALSE,
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'pending_approval', 'approved', 'active', 'rejected')),
    requested_by VARCHAR NOT NULL,
    approved_by VARCHAR,
    reason TEXT,
    recommendation_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_pricing_price_versions
    ON pricing_price_versions (tenant_id, product_id, version);
CREATE INDEX IF NOT EXISTS idx_pricing_price_versions_tenant
    ON pricing_price_versions (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_price_versions_status
    ON pricing_price_versions (tenant_id, product_id, status);

-- Approval decisions on price versions (append-only history).
CREATE TABLE IF NOT EXISTS pricing_price_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    price_version_id UUID NOT NULL REFERENCES pricing_price_versions (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    approver VARCHAR NOT NULL,
    decision VARCHAR NOT NULL CHECK (decision IN ('approved', 'rejected')),
    reason TEXT,
    decided_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pricing_price_approvals_version
    ON pricing_price_approvals (price_version_id);

-- Computed proration records (mid-cycle upgrade/downgrade, plan and
-- seat changes). The daily rate is rounded half-up to the minor
-- unit first, then multiplied by whole days.
CREATE TABLE IF NOT EXISTS pricing_proration_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    subscription_id VARCHAR,
    customer_id VARCHAR,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('upgrade', 'downgrade', 'plan_change', 'seat_change')),
    basis VARCHAR NOT NULL CHECK (basis IN ('per_day', 'per_month')),
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    change_at TIMESTAMPTZ NOT NULL,
    period_days BIGINT NOT NULL CHECK (period_days >= 1),
    days_used BIGINT NOT NULL CHECK (days_used >= 0),
    days_remaining BIGINT NOT NULL CHECK (days_remaining >= 0),
    old_daily_rate_scaled NUMERIC(19,4) NOT NULL
        CHECK (old_daily_rate_scaled >= 0),
    new_daily_rate_scaled NUMERIC(19,4) NOT NULL
        CHECK (new_daily_rate_scaled >= 0),
    unused_credit_scaled NUMERIC(19,4) NOT NULL
        CHECK (unused_credit_scaled >= 0),
    new_charge_scaled NUMERIC(19,4) NOT NULL
        CHECK (new_charge_scaled >= 0),
    net_due_scaled NUMERIC(19,4) NOT NULL,
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    lines JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (period_end > period_start),
    CHECK (change_at >= period_start AND change_at <= period_end)
);

CREATE INDEX IF NOT EXISTS idx_pricing_proration_tenant
    ON pricing_proration_records (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_proration_subscription
    ON pricing_proration_records (tenant_id, subscription_id);

-- Credit notes: the only way to correct an issued invoice. Issued
-- notes are immutable; corrections never edit the invoice itself.
CREATE TABLE IF NOT EXISTS pricing_credit_notes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    number VARCHAR NOT NULL,
    invoice_id VARCHAR NOT NULL,
    amount_scaled NUMERIC(19,4) NOT NULL CHECK (amount_scaled >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    lines JSONB NOT NULL DEFAULT '[]'::jsonb,
    reason TEXT NOT NULL DEFAULT '',
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'issued')),
    reconciled BOOLEAN NOT NULL DEFAULT FALSE,
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_pricing_credit_notes_number
    ON pricing_credit_notes (tenant_id, number);
CREATE INDEX IF NOT EXISTS idx_pricing_credit_notes_invoice
    ON pricing_credit_notes (tenant_id, invoice_id);

-- Stored reconciliation runs (invoice validation before issuing).
CREATE TABLE IF NOT EXISTS pricing_reconciliation_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    invoice_id VARCHAR,
    matched BOOLEAN NOT NULL,
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (currency ~ '^[A-Z]{3}$'),
    components JSONB NOT NULL DEFAULT '[]'::jsonb,
    total_discrepancy_scaled NUMERIC(19,4) NOT NULL DEFAULT 0,
    checked_by VARCHAR NOT NULL,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pricing_reconciliation_tenant
    ON pricing_reconciliation_reports (tenant_id);
CREATE INDEX IF NOT EXISTS idx_pricing_reconciliation_invoice
    ON pricing_reconciliation_reports (tenant_id, invoice_id);
