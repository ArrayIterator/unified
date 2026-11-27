-- Digital Products module tables for the Unified Pentagonal Project
-- (`src/modules/store.rs`, digital products extension): the
-- admin-extensible digital type registry, digital profiles on store
-- products, their deliverable files and free previews, the per-sale
-- royalty ledger, royalty payout requests, the per-buyer download
-- history, and the watermark format registry. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the store module header):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `product_id` / `payee` / `buyer_user_id` / `requested_by`
--     are TEXT with NO foreign key on purpose: product ids and
--     actor ids in this codebase are opaque strings (the store
--     product id, the `X-User-Id` header), not necessarily the UUID
--     primary key of another table. `payee_company_id` is TEXT for
--     the same reason (store company ids are opaque strings).
--   - Foreign keys exist only between tables created by this
--     migration. `digital_royalties` and `digital_downloads`
--     reference `digital_products` with ON DELETE RESTRICT, matching
--     the module rule that a product with royalty or download
--     history cannot be deleted; files and previews cascade with
--     their product. `digital_royalties.payout_id` is
--     ON DELETE SET NULL: deleting a payout request never deletes
--     the ledger rows it grouped.
--   - `digital_product_files.id` doubles as the `release_id` of
--     the signed download flow, and `digital_downloads.grant_id`
--     carries the purchase grant id (the `license_id` of the
--     entitlement). Digital products have no license; the grant id
--     is an opaque token, never a secret.
--   - `digital_product_files.checksum` holds a hex digest only,
--     never file content. Storage references (`storage_file_id`,
--     `cover_storage_id`) are tokenized ids, never file bytes.
--   - Watermarking is metadata-level only (see the module docs):
--     `digital_downloads.watermark_applied` records whether the
--     buyer identity was embedded into the delivered file's
--     metadata, and `watermark_formats` records which formats
--     support metadata at all. Nothing here claims unbreakable DRM.
--   - Money columns are NUMERIC(19,4). All ids are UUID except the
--     type registry and watermark registry keys, which are the
--     slugs the module uses (`ebook`, `pdf`, ...). All timestamps
--     are Timestamptz. Array columns are TEXT[]: `formats`,
--     `metadata_fields`, `watermark_formats`, `royalty_ids`.
--   - The five default digital types and the seven default
--     watermark formats are seeded below as data rows with the same
--     values the module seeds in memory, so the database copy
--     matches the runtime seed. They are configurable defaults:
--     administrators can change them through the API.
--   - The digital defaults themselves (default download limit,
--     sample rate limit, default royalty, watermark switch) have no
--     table yet and stay runtime-only until a settings table lands.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Admin-extensible digital product type registry (seeded defaults).
CREATE TABLE IF NOT EXISTS digital_product_types (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 80),
    description TEXT NOT NULL DEFAULT '' CHECK (char_length(description) <= 500),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO digital_product_types (id, label, description, enabled) VALUES
    ('ebook', 'Ebook', 'A digital book delivered as a document file.', TRUE),
    ('audiobook', 'Audiobook', 'A narrated audio edition of a book.', TRUE),
    ('template', 'Template', 'A reusable document, design, or code template.', TRUE),
    ('course-pack', 'Course pack', 'A bundled set of course materials.', TRUE),
    ('digital-asset', 'Digital asset', 'A downloadable digital asset such as graphics, fonts, or data.', TRUE)
ON CONFLICT (id) DO NOTHING;

-- Digital profile on a store product.
CREATE TABLE IF NOT EXISTS digital_products (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id TEXT NOT NULL,
    type_id TEXT NOT NULL REFERENCES digital_product_types (id) ON DELETE RESTRICT,
    author TEXT,
    publisher TEXT,
    edition TEXT,
    isbn TEXT,
    language TEXT,
    pages INTEGER CHECK (pages IS NULL OR pages >= 1),
    formats TEXT[] NOT NULL DEFAULT '{}',
    cover_storage_id TEXT,
    sample_allowed BOOLEAN NOT NULL DEFAULT FALSE,
    download_limit INTEGER NOT NULL DEFAULT 0 CHECK (download_limit BETWEEN 0 AND 10000),
    royalty_kind TEXT NOT NULL DEFAULT 'percent' CHECK (royalty_kind IN ('percent', 'fixed')),
    royalty_value NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (royalty_value >= 0),
    royalty_payee TEXT,
    royalty_payee_company_id TEXT,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT digital_products_product_id_unique UNIQUE (product_id)
);

CREATE INDEX IF NOT EXISTS digital_products_type_id_idx
    ON digital_products (type_id);
CREATE INDEX IF NOT EXISTS digital_products_status_idx
    ON digital_products (status);

-- Deliverable files of a digital product (storage references only).
CREATE TABLE IF NOT EXISTS digital_product_files (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    digital_product_id UUID NOT NULL REFERENCES digital_products (id) ON DELETE CASCADE,
    storage_file_id TEXT NOT NULL CHECK (char_length(storage_file_id) BETWEEN 1 AND 512),
    format TEXT NOT NULL CHECK (char_length(format) BETWEEN 1 AND 12),
    label TEXT,
    file_size_bytes BIGINT CHECK (file_size_bytes IS NULL OR file_size_bytes > 0),
    checksum TEXT CHECK (checksum IS NULL OR char_length(checksum) BETWEEN 32 AND 128),
    is_primary BOOLEAN NOT NULL DEFAULT FALSE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS digital_product_files_product_idx
    ON digital_product_files (digital_product_id);

-- Free excerpt / sample files of a digital product.
CREATE TABLE IF NOT EXISTS digital_product_previews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    digital_product_id UUID NOT NULL REFERENCES digital_products (id) ON DELETE CASCADE,
    storage_file_id TEXT NOT NULL CHECK (char_length(storage_file_id) BETWEEN 1 AND 512),
    format TEXT NOT NULL CHECK (char_length(format) BETWEEN 1 AND 12),
    label TEXT,
    allowed BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS digital_product_previews_product_idx
    ON digital_product_previews (digital_product_id);

-- Royalty payout requests. Money moves in billing; this table only
-- groups royalty rows into a request and tracks its lifecycle.
CREATE TABLE IF NOT EXISTS digital_payout_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payee TEXT,
    payee_company_id TEXT,
    currency CHAR(3) NOT NULL,
    amount NUMERIC(19,4) NOT NULL CHECK (amount >= 0),
    royalty_ids UUID[] NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'requested' CHECK (status IN ('requested', 'approved', 'paid', 'rejected')),
    requested_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT digital_payout_requests_payee_present CHECK (payee IS NOT NULL OR payee_company_id IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS digital_payout_requests_payee_idx
    ON digital_payout_requests (payee);
CREATE INDEX IF NOT EXISTS digital_payout_requests_status_idx
    ON digital_payout_requests (status);

-- Per-sale royalty ledger: one row per digital product per order.
CREATE TABLE IF NOT EXISTS digital_royalties (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    digital_product_id UUID NOT NULL REFERENCES digital_products (id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL,
    order_id TEXT NOT NULL,
    buyer_user_id TEXT NOT NULL,
    payee TEXT,
    payee_company_id TEXT,
    amount NUMERIC(19,4) NOT NULL CHECK (amount >= 0),
    currency CHAR(3) NOT NULL,
    royalty_kind TEXT NOT NULL CHECK (royalty_kind IN ('percent', 'fixed')),
    royalty_value NUMERIC(19,4) NOT NULL CHECK (royalty_value >= 0),
    status TEXT NOT NULL DEFAULT 'accrued' CHECK (status IN ('accrued', 'payout_requested', 'approved', 'paid', 'rejected', 'reversed')),
    payout_id UUID REFERENCES digital_payout_requests (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT digital_royalties_order_product_unique UNIQUE (order_id, digital_product_id)
);

CREATE INDEX IF NOT EXISTS digital_royalties_payee_idx
    ON digital_royalties (payee);
CREATE INDEX IF NOT EXISTS digital_royalties_status_idx
    ON digital_royalties (status);
CREATE INDEX IF NOT EXISTS digital_royalties_order_idx
    ON digital_royalties (order_id);

-- Per-buyer download history: purchases and free samples alike.
CREATE TABLE IF NOT EXISTS digital_downloads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    digital_product_id UUID NOT NULL REFERENCES digital_products (id) ON DELETE RESTRICT,
    file_id UUID REFERENCES digital_product_files (id) ON DELETE SET NULL,
    preview_id UUID REFERENCES digital_product_previews (id) ON DELETE SET NULL,
    buyer_user_id TEXT NOT NULL,
    order_id TEXT,
    grant_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('purchase', 'sample')),
    watermark_subject TEXT,
    watermark_applied BOOLEAN NOT NULL DEFAULT FALSE,
    watermark_formats TEXT[] NOT NULL DEFAULT '{}',
    downloaded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS digital_downloads_product_idx
    ON digital_downloads (digital_product_id);
CREATE INDEX IF NOT EXISTS digital_downloads_buyer_idx
    ON digital_downloads (buyer_user_id);
CREATE INDEX IF NOT EXISTS digital_downloads_order_idx
    ON digital_downloads (order_id);

-- Watermark format registry: which formats support buyer-identity
-- metadata (seeded defaults).
CREATE TABLE IF NOT EXISTS digital_watermark_formats (
    format TEXT PRIMARY KEY,
    metadata_supported BOOLEAN NOT NULL DEFAULT FALSE,
    metadata_fields TEXT[] NOT NULL DEFAULT '{}',
    notes TEXT NOT NULL DEFAULT '' CHECK (char_length(notes) <= 500),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO digital_watermark_formats (format, metadata_supported, metadata_fields, notes) VALUES
    ('pdf', TRUE, ARRAY['Author', 'Creator', 'Keywords'], 'Buyer identity is written into the PDF document information dictionary.'),
    ('epub', TRUE, ARRAY['dc:identifier', 'dc:creator'], 'Buyer identity is written into the EPUB package metadata.'),
    ('mobi', TRUE, ARRAY['EXTH author', 'EXTH identifier'], 'Buyer identity is written into the MOBI EXTH records where the writer supports them.'),
    ('mp3', TRUE, ARRAY['ID3 comment', 'ID3 artist'], 'Buyer identity is written into the ID3 comment frame.'),
    ('m4b', TRUE, ARRAY['copyright', 'comment'], 'Buyer identity is written into the audiobook metadata atoms.'),
    ('mp4', TRUE, ARRAY['copyright', 'comment'], 'Buyer identity is written into the video metadata atoms.'),
    ('zip', FALSE, ARRAY[]::TEXT[], 'Archives carry no standard document metadata; the buyer is recorded in the delivery manifest instead.')
ON CONFLICT (format) DO NOTHING;
