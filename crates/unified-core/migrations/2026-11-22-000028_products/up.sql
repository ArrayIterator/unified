-- Product Management tables for the Unified Pentagonal Project
-- (`src/modules/products.rs`): the single source of truth for "what
-- we sell" - products, variants and options, bundles, pricing
-- (tiers, regional prices, usage prices, tax rules), inventory,
-- product files and versions, taxonomy and tags.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - Money is NUMERIC(19,4) everywhere, never DOUBLE PRECISION
--     (see migration 000003). Amounts are integer minor units in
--     code (`Money.amount_minor`); each `Money` field maps to two
--     columns, an amount and its ISO 4217 currency: `price` +
--     `price_currency`, `renewal_price` + `renewal_price_currency`,
--     and `unit_price` + `currency` on usage prices. A renewal
--     currency falls back to the product price currency in code, but
--     a caller-supplied currency is honoured, so the renewal
--     currency is stored per field rather than shared.
--   - `products.price_delta` (variants) is NUMERIC(19,4) with no
--     non-negative CHECK: a variant price delta may be negative by
--     design. `products.bundle_discount_bps` is basis points
--     (100 = 1%), bounded to 0..10000.
--   - `products.license_template_*` flattens the `LicenseTemplate`
--     value object (template name, seats, download limit, duration
--     in days, requires-license flag). The module validates seats
--     and download limit at 1 or more and duration at 1 day or
--     more when present, hence the CHECKs.
--   - `products.vendor_company_id`, `products.software_id`, and
--     `products.support_department_id` are plain UUID references
--     with NO foreign key, exactly as the module states them: the
--     vendor is validated through the `company.exists` hook "when a
--     handler is registered, otherwise accepted as a plain
--     reference"; the software id is "a reference only" into the
--     Software Manager; the support department id is "a plain
--     reference; the departments module owns departments".
--     `products.group_id` / `category_id` DO carry foreign keys to
--     `product_taxonomy`: taxonomy nodes are owned by this module,
--     mirroring `software.group_id` / `category_id` in 000002.
--   - `product_files` and `product_versions` reference Software
--     Manager records only (`software_id`, `release_id`, `file_id`
--     are plain UUID references with no foreign key, like the
--     `file_id` columns in 000002 whose identity is unresolved).
--     This module never stores, serves, or verifies file bytes.
--   - `product_taxonomy` mirrors `software_taxonomy` (000002): same
--     kinds (`group`, `category`), same parent ON DELETE CASCADE,
--     same unique index on (kind, parent, slug) with COALESCE so
--     root nodes (NULL parent) are covered.
--   - `product_inventory` allows at most one row per product at
--     product level and one row per (product, variant): the two
--     partial unique indexes enforce that, because a plain unique
--     index treats NULL variant ids as distinct.
--   - `product_tax_rules.rate_bps` is basis points (100 = 1%).
--     `country` is an ISO 3166-1 alpha-2 code or `GLOBAL`, the
--     same shape as `product_regional_prices.region`.
--   - Status / enum value lists are the module constants
--     (`PRODUCT_KINDS`, `PRODUCT_STATUSES`, `PRICING_MODELS`,
--     `BILLING_PERIODS`, `BUNDLE_RULES`, `TAXONOMY_KINDS`).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Product taxonomy: groups and categories (cycle-safe tree), in this
-- module's own namespace. Created before `products` because
-- `products.group_id` / `category_id` reference it.
CREATE TABLE IF NOT EXISTS product_taxonomy (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind VARCHAR NOT NULL CHECK (kind IN ('group', 'category')),
    slug VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    parent_id UUID REFERENCES product_taxonomy (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    sort_order BIGINT NOT NULL DEFAULT 0,
    depth BIGINT NOT NULL DEFAULT 0 CHECK (depth >= 0),
    path VARCHAR NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_product_taxonomy_parent_id
    ON product_taxonomy (parent_id);
-- One slug per (kind, parent); COALESCE so root nodes (NULL parent)
-- are covered (plain UNIQUE treats NULLs as distinct).
CREATE UNIQUE INDEX IF NOT EXISTS uq_product_taxonomy_kind_parent_slug
    ON product_taxonomy (
        kind,
        COALESCE(parent_id, '00000000-0000-0000-0000-000000000000'::uuid),
        slug
    );

-- One product: the single source of truth for what is sold.
CREATE TABLE IF NOT EXISTS products (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    kind VARCHAR NOT NULL
        CHECK (kind IN ('digital', 'physical', 'service', 'subscription', 'bundle', 'meeting', 'hosting')),
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'deprecated', 'retired')),
    pricing_model VARCHAR NOT NULL DEFAULT 'one_time'
        CHECK (pricing_model IN ('one_time', 'subscription', 'usage', 'per_seat')),
    billing_period VARCHAR
        CHECK (billing_period IS NULL OR billing_period IN ('monthly', 'yearly', 'weekly')),
    price NUMERIC(19,4) NOT NULL CHECK (price >= 0),
    price_currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (char_length(price_currency) = 3 AND price_currency ~ '^[A-Za-z]{3}$'),
    renewal_price NUMERIC(19,4) CHECK (renewal_price IS NULL OR renewal_price >= 0),
    renewal_price_currency VARCHAR
        CHECK (renewal_price_currency IS NULL OR (char_length(renewal_price_currency) = 3 AND renewal_price_currency ~ '^[A-Za-z]{3}$')),
    trial_days INTEGER NOT NULL DEFAULT 0 CHECK (trial_days >= 0),
    vendor_company_id UUID,
    group_id UUID REFERENCES product_taxonomy (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    category_id UUID REFERENCES product_taxonomy (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    software_id UUID,
    license_template_name VARCHAR,
    license_seats INTEGER NOT NULL DEFAULT 1 CHECK (license_seats >= 1),
    license_download_limit INTEGER NOT NULL DEFAULT 10 CHECK (license_download_limit >= 1),
    license_duration_days BIGINT CHECK (license_duration_days IS NULL OR license_duration_days >= 1),
    license_requires_license BOOLEAN NOT NULL DEFAULT FALSE,
    support_department_id UUID,
    support_department_name VARCHAR,
    bundle_rule VARCHAR NOT NULL DEFAULT 'sum'
        CHECK (bundle_rule IN ('sum', 'fixed', 'discount_percent')),
    bundle_discount_bps BIGINT
        CHECK (bundle_discount_bps IS NULL OR (bundle_discount_bps >= 0 AND bundle_discount_bps <= 10000)),
    listed BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_products_slug
    ON products (slug);
CREATE INDEX IF NOT EXISTS idx_products_kind
    ON products (kind);
CREATE INDEX IF NOT EXISTS idx_products_status
    ON products (status);
CREATE INDEX IF NOT EXISTS idx_products_vendor_company_id
    ON products (vendor_company_id);
CREATE INDEX IF NOT EXISTS idx_products_group_id
    ON products (group_id);
CREATE INDEX IF NOT EXISTS idx_products_category_id
    ON products (category_id);
CREATE INDEX IF NOT EXISTS idx_products_software_id
    ON products (software_id);
CREATE INDEX IF NOT EXISTS idx_products_support_department_id
    ON products (support_department_id);
CREATE INDEX IF NOT EXISTS idx_products_listed
    ON products (listed);


-- One configurable option of a product (for example `plan`, `size`)
-- with its allowed values.
CREATE TABLE IF NOT EXISTS product_options (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR NOT NULL,
    values JSONB NOT NULL DEFAULT '[]'::jsonb,
    required BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, name)
);

CREATE INDEX IF NOT EXISTS idx_product_options_product_id
    ON product_options (product_id);

-- One sellable variant: a combination of option values with its own
-- SKU and a price delta (minor units, may be negative) on top of
-- the product base price.
CREATE TABLE IF NOT EXISTS product_variants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    sku VARCHAR NOT NULL,
    label VARCHAR NOT NULL DEFAULT '',
    option_values JSONB NOT NULL DEFAULT '{}'::jsonb,
    price_delta NUMERIC(19,4) NOT NULL DEFAULT 0,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, sku)
);

CREATE INDEX IF NOT EXISTS idx_product_variants_product_id
    ON product_variants (product_id);

-- One product contained in a bundle, with its quantity and an
-- optional per-item price override (minor units) that replaces the
-- child product's base price inside this bundle.
CREATE TABLE IF NOT EXISTS product_bundle_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bundle_product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    child_product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    quantity INTEGER NOT NULL DEFAULT 1 CHECK (quantity >= 1),
    price_override NUMERIC(19,4) CHECK (price_override IS NULL OR price_override >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (bundle_product_id, child_product_id)
);

CREATE INDEX IF NOT EXISTS idx_product_bundle_items_bundle_product_id
    ON product_bundle_items (bundle_product_id);
CREATE INDEX IF NOT EXISTS idx_product_bundle_items_child_product_id
    ON product_bundle_items (child_product_id);

-- One pricing tier / edition of a product, with its own price, seat
-- count, trial, renewal terms, and feature list.
CREATE TABLE IF NOT EXISTS product_price_tiers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    slug VARCHAR NOT NULL,
    label VARCHAR NOT NULL DEFAULT '',
    price NUMERIC(19,4) NOT NULL CHECK (price >= 0),
    price_currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (char_length(price_currency) = 3 AND price_currency ~ '^[A-Za-z]{3}$'),
    renewal_price NUMERIC(19,4) CHECK (renewal_price IS NULL OR renewal_price >= 0),
    renewal_price_currency VARCHAR
        CHECK (renewal_price_currency IS NULL OR (char_length(renewal_price_currency) = 3 AND renewal_price_currency ~ '^[A-Za-z]{3}$')),
    billing_period VARCHAR
        CHECK (billing_period IS NULL OR billing_period IN ('monthly', 'yearly', 'weekly')),
    trial_days INTEGER NOT NULL DEFAULT 0 CHECK (trial_days >= 0),
    seats INTEGER NOT NULL DEFAULT 1 CHECK (seats >= 1),
    per_seat BOOLEAN NOT NULL DEFAULT FALSE,
    download_limit INTEGER NOT NULL DEFAULT 10 CHECK (download_limit >= 0),
    features JSONB NOT NULL DEFAULT '[]'::jsonb,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_product_price_tiers_product_id
    ON product_price_tiers (product_id);

-- A regional price override: the price of a product in one country
-- (ISO 3166-1 alpha-2 code) or `GLOBAL` as the fallback.
CREATE TABLE IF NOT EXISTS product_regional_prices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    region VARCHAR NOT NULL
        CHECK (region = 'GLOBAL' OR (char_length(region) = 2 AND region ~ '^[A-Za-z]{2}$')),
    price NUMERIC(19,4) NOT NULL CHECK (price >= 0),
    price_currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (char_length(price_currency) = 3 AND price_currency ~ '^[A-Za-z]{3}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, region)
);

CREATE INDEX IF NOT EXISTS idx_product_regional_prices_product_id
    ON product_regional_prices (product_id);

-- A usage-based price: a metered metric charged per unit after the
-- included allowance, on top of the product base price.
CREATE TABLE IF NOT EXISTS product_usage_prices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    metric VARCHAR NOT NULL,
    unit_price NUMERIC(19,4) NOT NULL CHECK (unit_price >= 0),
    currency VARCHAR NOT NULL DEFAULT 'USD'
        CHECK (char_length(currency) = 3 AND currency ~ '^[A-Za-z]{3}$'),
    included_units BIGINT NOT NULL DEFAULT 0 CHECK (included_units >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, metric)
);

CREATE INDEX IF NOT EXISTS idx_product_usage_prices_product_id
    ON product_usage_prices (product_id);

-- A tax rule: a rate in basis points (100 = 1%) for one country
-- (ISO 3166-1 alpha-2) or `GLOBAL` as the fallback rate.
CREATE TABLE IF NOT EXISTS product_tax_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    country VARCHAR NOT NULL
        CHECK (country = 'GLOBAL' OR (char_length(country) = 2 AND country ~ '^[A-Za-z]{2}$')),
    name VARCHAR NOT NULL,
    rate_bps BIGINT NOT NULL CHECK (rate_bps >= 0 AND rate_bps <= 10000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_product_tax_rules_country
    ON product_tax_rules (country);

-- Stock for a physical product (or one of its variants).
-- `reserved` never exceeds `quantity`; available is always
-- `quantity - reserved`.
CREATE TABLE IF NOT EXISTS product_inventory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    variant_id UUID REFERENCES product_variants (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    quantity BIGINT NOT NULL DEFAULT 0 CHECK (quantity >= 0),
    reserved BIGINT NOT NULL DEFAULT 0
        CHECK (reserved >= 0 AND reserved <= quantity),
    low_stock_threshold BIGINT NOT NULL DEFAULT 0 CHECK (low_stock_threshold >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_product_inventory_product_id
    ON product_inventory (product_id);
CREATE INDEX IF NOT EXISTS idx_product_inventory_variant_id
    ON product_inventory (variant_id);
-- At most one product-level row (NULL variant) and one row per
-- (product, variant): a plain unique index treats NULLs as
-- distinct, so the two partial indexes are needed.
CREATE UNIQUE INDEX IF NOT EXISTS uq_product_inventory_product
    ON product_inventory (product_id) WHERE variant_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uq_product_inventory_product_variant
    ON product_inventory (product_id, variant_id) WHERE variant_id IS NOT NULL;

-- A file attached to a product version. This record only references
-- a software release / file owned by the Software Manager; this
-- module never stores, serves, or verifies file bytes.
CREATE TABLE IF NOT EXISTS product_files (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    software_id UUID,
    software_version VARCHAR,
    release_id UUID,
    file_id UUID,
    filename VARCHAR,
    checksum_sha256 VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_product_files_product_id
    ON product_files (product_id);

-- One released version of a product, referencing a software version
-- / release when the product sells software.
CREATE TABLE IF NOT EXISTS product_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    version VARCHAR NOT NULL,
    software_id UUID,
    release_id UUID,
    changelog TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (product_id, version)
);

CREATE INDEX IF NOT EXISTS idx_product_versions_product_id
    ON product_versions (product_id);

-- One product tag.
CREATE TABLE IF NOT EXISTS product_tags (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_product_tags_slug
    ON product_tags (slug);

-- Tag assignments to products (many-to-many).
CREATE TABLE IF NOT EXISTS product_tag_assignments (
    product_id UUID NOT NULL REFERENCES products (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    tag_id UUID NOT NULL REFERENCES product_tags (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (product_id, tag_id)
);

CREATE INDEX IF NOT EXISTS idx_product_tag_assignments_tag_id
    ON product_tag_assignments (tag_id);
