-- Printing tables for the Unified project
-- (`src/modules/printing.rs` + the invoice-template and print wiring
-- in `src/modules/billing.rs`): paper sizes (standard sizes seeded
-- as data, custom sizes admin-editable), invoice templates with
-- version history, thermal printer profiles, and the print-job queue.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap stores are still
-- authoritative; see TODO.md):
--   - `invoice_templates.company_id`, `paper_sizes.company_id`,
--     `printer_profiles.company_id`, and `print_jobs.company_id`
--     are plain VARCHAR company ids, exactly like
--     `store_orders.company_id` and the billing invoice records:
--     the modules identify companies by id string and never join
--     to a companies table from here.
--   - `invoice_templates.logo_storage_target` +
--     `logo_storage_key` reference bytes that live in the storage
--     module; `logo_url` is an optional directly usable URL.
--     No logo bytes are stored in this migration.
--   - `invoice_templates.columns` is a JSONB array of
--     `{"key", "label", "visible"}` objects; the allowed column
--     keys are enforced by the billing module, not by a CHECK
--     here, so a new column key never needs a migration.
--   - `invoice_templates.line_item_layout` is a JSONB object of
--     layout options (row density, tax column placement, and
--     similar); it is free-form on purpose for the same reason.
--   - `invoice_template_versions.snapshot` is the full JSON
--     rendering of one template version. Every edit archives the
--     previous version before bumping `invoice_templates.version`,
--     so history is append-only and any version can be restored
--     as a new version.
--   - Only one default template per company is allowed: the two
--     partial unique indexes on `invoice_templates.is_default`
--     enforce it separately for company templates and for the
--     global (NULL company) template. The same shape guards the
--     single default printer profile per company / globally.
--   - `paper_sizes` holds BOTH the standard sizes and the custom
--     ones. Standard rows (`is_standard = true`, `company_id`
--     NULL) are seeded below: ISO 216 A0-A6, Letter, Legal,
--     Tabloid, and the common thermal roll widths 58mm and 80mm.
--     Thermal rows have a NULL height (continuous roll) and a
--     `thermal_width_mm`. Custom rows are admin-created and carry
--     a `company_id` (or NULL for a global custom size).
--   - `printer_profiles.auth_token_ref` is a reference to a
--     credential held by the vault / secrets store. It is never
--     a secret itself: no password, token, or key is stored here.
--   - `print_jobs.status` is `queued`, `printing`, `done`, or
--     `failed` (`PrintJobStatus`). `document_ref` is the storage
--     reference of the rendered print-ready document when one was
--     persisted; `escpos_preview` is the human-readable text
--     rendering of a thermal job. The ESC/POS byte stream itself
--     lives in the in-memory job record (it is regenerated from
--     the stored lines on reprint) and is not persisted here.
--   - `print_jobs.output_kind` is `paper`, `thermal`, `pdf`, or
--     `email`. A `pdf` job renders the print-ready HTML document
--     with `@page` rules for the chosen paper size; binary PDF
--     conversion is a deployment step (headless browser or a
--     configured converter hook), not a column in this table.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Paper sizes: standard sizes seeded, custom sizes admin-editable.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS paper_sizes (
    code VARCHAR PRIMARY KEY,
    name VARCHAR NOT NULL,
    width_mm NUMERIC(8, 2) NOT NULL CHECK (width_mm > 0),
    height_mm NUMERIC(8, 2) CHECK (height_mm IS NULL OR height_mm > 0),
    margin_top_mm NUMERIC(6, 2) NOT NULL DEFAULT 10
        CHECK (margin_top_mm >= 0),
    margin_bottom_mm NUMERIC(6, 2) NOT NULL DEFAULT 10
        CHECK (margin_bottom_mm >= 0),
    margin_left_mm NUMERIC(6, 2) NOT NULL DEFAULT 10
        CHECK (margin_left_mm >= 0),
    margin_right_mm NUMERIC(6, 2) NOT NULL DEFAULT 10
        CHECK (margin_right_mm >= 0),
    is_standard BOOLEAN NOT NULL DEFAULT FALSE,
    is_thermal BOOLEAN NOT NULL DEFAULT FALSE,
    thermal_width_mm INTEGER
        CHECK (thermal_width_mm IS NULL OR thermal_width_mm > 0),
    company_id VARCHAR,
    created_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (is_thermal = FALSE OR height_mm IS NULL),
    CHECK (is_thermal = FALSE OR thermal_width_mm IS NOT NULL),
    CHECK (is_thermal = TRUE OR thermal_width_mm IS NULL),
    CHECK (margin_left_mm + margin_right_mm < width_mm),
    CHECK (height_mm IS NULL
        OR margin_top_mm + margin_bottom_mm < height_mm)
);

CREATE INDEX IF NOT EXISTS idx_paper_sizes_company_id
    ON paper_sizes (company_id);
CREATE INDEX IF NOT EXISTS idx_paper_sizes_is_standard
    ON paper_sizes (is_standard);

-- Standard sizes (ISO 216 A-series, Letter, Legal, Tabloid, and the
-- common thermal roll widths). These are international standards, so
-- they are seeded as data rows; the module also carries the same
-- values as constants for the in-memory store.
INSERT INTO paper_sizes
    (code, name, width_mm, height_mm, margin_top_mm, margin_bottom_mm,
     margin_left_mm, margin_right_mm, is_standard, is_thermal,
     thermal_width_mm, company_id)
VALUES
    ('a0', 'A0', 841.00, 1189.00, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('a1', 'A1', 594.00, 841.00, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('a2', 'A2', 420.00, 594.00, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('a3', 'A3', 297.00, 420.00, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('a4', 'A4', 210.00, 297.00, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('a5', 'A5', 148.00, 210.00, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('a6', 'A6', 105.00, 148.00, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('letter', 'Letter', 215.90, 279.40, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('legal', 'Legal', 215.90, 355.60, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('tabloid', 'Tabloid', 279.40, 431.80, 10, 10, 10, 10, TRUE, FALSE, NULL, NULL),
    ('thermal-58', 'Thermal 58mm', 58.00, NULL, 2, 2, 2, 2, TRUE, TRUE, 58, NULL),
    ('thermal-80', 'Thermal 80mm', 80.00, NULL, 2, 2, 2, 2, TRUE, TRUE, 80, NULL)
ON CONFLICT (code) DO NOTHING;

-- ---------------------------------------------------------------------------
-- One invoice template: header/footer, logo reference, colours,
-- columns, line-item layout, tax breakdown flag, payment
-- instructions, footer notes. Templates are data, never hard-coded
-- layouts; `version` bumps on every edit.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invoice_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR,
    name VARCHAR NOT NULL CHECK (char_length(name) >= 1),
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    paper_size_code VARCHAR REFERENCES paper_sizes (code)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    header_text TEXT,
    footer_text TEXT,
    logo_storage_target VARCHAR,
    logo_storage_key VARCHAR,
    logo_url VARCHAR,
    primary_color VARCHAR(7) NOT NULL DEFAULT '#111827',
    accent_color VARCHAR(7) NOT NULL DEFAULT '#2563EB',
    font_family VARCHAR NOT NULL DEFAULT 'Arial, sans-serif',
    columns JSONB NOT NULL DEFAULT '[]'::jsonb,
    line_item_layout JSONB NOT NULL DEFAULT '{}'::jsonb,
    show_tax_breakdown BOOLEAN NOT NULL DEFAULT TRUE,
    payment_instructions TEXT,
    footer_notes TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_invoice_templates_company_id
    ON invoice_templates (company_id);
CREATE INDEX IF NOT EXISTS idx_invoice_templates_paper_size_code
    ON invoice_templates (paper_size_code);
CREATE UNIQUE INDEX IF NOT EXISTS uq_invoice_templates_default_company
    ON invoice_templates (company_id)
    WHERE is_default AND company_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uq_invoice_templates_default_global
    ON invoice_templates ((TRUE))
    WHERE is_default AND company_id IS NULL;

-- ---------------------------------------------------------------------------
-- One archived template version: the full JSON snapshot of the
-- template as it was before an edit. Append-only.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS invoice_template_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES invoice_templates (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    version INTEGER NOT NULL CHECK (version >= 1),
    snapshot JSONB NOT NULL,
    created_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (template_id, version)
);

CREATE INDEX IF NOT EXISTS idx_invoice_template_versions_template_id
    ON invoice_template_versions (template_id);

-- ---------------------------------------------------------------------------
-- One thermal printer profile: connection, width, character set,
-- code page, and capabilities. The profile describes how to reach
-- the printer; the server emits the ESC/POS stream, it does not
-- claim a direct hardware driver.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS printer_profiles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR,
    name VARCHAR NOT NULL CHECK (char_length(name) >= 1),
    connection_type VARCHAR NOT NULL DEFAULT 'network'
        CHECK (connection_type IN ('network', 'usb', 'serial', 'file', 'cloud')),
    connection_target VARCHAR,
    auth_token_ref VARCHAR,
    width_columns INTEGER NOT NULL DEFAULT 42
        CHECK (width_columns >= 16 AND width_columns <= 120),
    character_set VARCHAR NOT NULL DEFAULT 'cp437',
    code_page VARCHAR NOT NULL DEFAULT 'cp437',
    supports_drawer_kick BOOLEAN NOT NULL DEFAULT FALSE,
    supports_cut BOOLEAN NOT NULL DEFAULT TRUE,
    paper_size_code VARCHAR REFERENCES paper_sizes (code)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_printer_profiles_company_id
    ON printer_profiles (company_id);
CREATE INDEX IF NOT EXISTS idx_printer_profiles_paper_size_code
    ON printer_profiles (paper_size_code);
CREATE UNIQUE INDEX IF NOT EXISTS uq_printer_profiles_default_company
    ON printer_profiles (company_id)
    WHERE is_default AND company_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uq_printer_profiles_default_global
    ON printer_profiles ((TRUE))
    WHERE is_default AND company_id IS NULL;

-- ---------------------------------------------------------------------------
-- One print job: a queued request to print, email, or export one
-- invoice, its lifecycle, and its rendered output references.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS print_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR,
    invoice_id VARCHAR NOT NULL,
    template_id UUID REFERENCES invoice_templates (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    template_version INTEGER CHECK (template_version IS NULL OR template_version >= 1),
    paper_size_code VARCHAR REFERENCES paper_sizes (code)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    output_kind VARCHAR NOT NULL DEFAULT 'paper'
        CHECK (output_kind IN ('paper', 'thermal', 'pdf', 'email')),
    printer_profile_id UUID REFERENCES printer_profiles (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    status VARCHAR NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'printing', 'done', 'failed')),
    document_ref VARCHAR,
    escpos_preview TEXT,
    error_message TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    requested_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    CHECK (completed_at IS NULL OR started_at IS NULL OR completed_at >= started_at)
);

CREATE INDEX IF NOT EXISTS idx_print_jobs_invoice_id
    ON print_jobs (invoice_id);
CREATE INDEX IF NOT EXISTS idx_print_jobs_status
    ON print_jobs (status);
CREATE INDEX IF NOT EXISTS idx_print_jobs_company_id
    ON print_jobs (company_id);
CREATE INDEX IF NOT EXISTS idx_print_jobs_template_id
    ON print_jobs (template_id);
CREATE INDEX IF NOT EXISTS idx_print_jobs_printer_profile_id
    ON print_jobs (printer_profile_id);
