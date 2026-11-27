-- Form Builder tables for the Unified Pentagonal Project
-- (`src/modules/forms.rs`): form definitions, ordered fields with
-- validation rules and conditional visibility, submissions, and
-- submitted values.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/forms.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `forms.company_id` and `forms.owner_id` are plain VARCHAR
--     references: the module identifies callers by the `X-User-Id`
--     header and keeps company / user ids as strings, exactly like
--     `store_orders.user_id` in migration 000002. No foreign key
--     to `users` or `companies` is stored here.
--   - `forms.slug` is unique (lowercase letters, digits, single
--     hyphens; validated in the module). `forms.status` is `draft`,
--     `published`, or `archived` (`FormStatus`); only `published`
--     forms answer the public render and submit routes.
--   - `forms.honeypot_field` is the name of the honeypot field
--     (default `website`); it must never collide with a real field
--     key (enforced in the module).
--   - `forms.mapping` is JSONB holding the `FormMapping`
--     (`target` one of `crm_lead`, `support_ticket`, `task`;
--     `field_map` from form field keys to target field names;
--     optional `title_template` with `{field_key}`
--     placeholders). Mapping itself is NOT done by a table: the
--     module emits `forms.mapping.requested` and the owning module
--     (CRM, tickets, tasks) answers it. The outcome is recorded on
--     the submission (`mapping_status`, `mapping_error`,
--     `mapped_target_id`), never silently dropped.
--   - `form_fields.field_type` is one of `text`, `textarea`,
--     `email`, `number`, `select`, `multi-select`, `checkbox`,
--     `radio`, `date`, `file`, `rating`, `consent`. `position`
--     orders fields inside one form (unique per form) and is the
--     drag-drop order. `key` is `[a-z0-9_]+`, unique per form.
--   - Validation rules are type-scoped in the module and mirrored
--     by the CHECKs below: `options` (allowed values) only for
--     `select` / `multi-select` / `radio`; `min_length` /
--     `max_length` / `pattern` only for `text` / `textarea` /
--     `email`; `min_value` / `max_value` only for `number`.
--     `pattern` must be a valid regex (checked in the module with
--     the `regex` crate; PostgreSQL cannot validate regex syntax
--     in a CHECK).
--   - `form_fields.show_when` is JSONB holding the
--     `FieldCondition` (`field_key`, `equals`): the field shows
--     only when the referenced field equals the given value.
--     Hidden fields are skipped by submit validation and never
--     stored. The referenced key must be another field of the same
--     form (enforced in the module).
--   - `form_submissions.status` is `new`, `read`, or `spam`.
--     `mapping_status` is `none`, `mapped`, or `failed`.
--     `ip_address` is stored for rate limiting and abuse review
--     only; it is never used for authentication.
--   - `form_submission_values.value` is JSONB holding the
--     validated submitted value (string, number, boolean, or
--     array for multi-select). `file_storage_key` is set for
--     `file` fields: the storage-module key reference submitted by
--     the client (the file itself lives in the storage module).
--   - Rate-limit attempts (10 per IP per form per hour, 200 per
--     form per hour) live only in memory; they are deliberately
--     NOT persisted, so a restart resets the counters.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One form definition.
CREATE TABLE IF NOT EXISTS forms (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    description TEXT,
    slug VARCHAR(128) NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'archived')),
    company_id VARCHAR(128),
    owner_id VARCHAR NOT NULL,
    submit_button_label VARCHAR(80) NOT NULL DEFAULT 'Submit',
    success_message VARCHAR(1000) NOT NULL DEFAULT 'Thank you for your submission.',
    honeypot_field VARCHAR(64) NOT NULL DEFAULT 'website',
    mapping JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (mapping IS NULL OR (mapping ->> 'target') IN ('crm_lead', 'support_ticket', 'task'))
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_forms_slug ON forms (slug);
CREATE INDEX IF NOT EXISTS idx_forms_status ON forms (status);
CREATE INDEX IF NOT EXISTS idx_forms_company_id ON forms (company_id);
CREATE INDEX IF NOT EXISTS idx_forms_owner_id ON forms (owner_id);

-- One field of a form.
CREATE TABLE IF NOT EXISTS form_fields (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    form_id UUID NOT NULL REFERENCES forms (id) ON DELETE CASCADE,
    key VARCHAR(64) NOT NULL,
    label VARCHAR(200) NOT NULL,
    field_type VARCHAR NOT NULL
        CHECK (field_type IN ('text', 'textarea', 'email', 'number', 'select', 'multi-select', 'checkbox', 'radio', 'date', 'file', 'rating', 'consent')),
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    required BOOLEAN NOT NULL DEFAULT FALSE,
    placeholder VARCHAR(200),
    help_text VARCHAR(500),
    options TEXT[] NOT NULL DEFAULT '{}',
    min_length BIGINT CHECK (min_length IS NULL OR min_length >= 0),
    max_length BIGINT CHECK (max_length IS NULL OR max_length >= 0),
    min_value DOUBLE PRECISION,
    max_value DOUBLE PRECISION,
    pattern VARCHAR(500),
    show_when JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (key ~ '^[a-z_][a-z0-9_]*$'),
    CHECK (field_type IN ('select', 'multi-select', 'radio') OR cardinality(options) = 0),
    CHECK (field_type IN ('text', 'textarea', 'email') OR (min_length IS NULL AND max_length IS NULL AND pattern IS NULL)),
    CHECK (field_type = 'number' OR (min_value IS NULL AND max_value IS NULL)),
    CHECK (max_length IS NULL OR min_length IS NULL OR max_length >= min_length),
    CHECK (max_value IS NULL OR min_value IS NULL OR max_value >= min_value)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_form_fields_form_key ON form_fields (form_id, key);
CREATE UNIQUE INDEX IF NOT EXISTS uq_form_fields_form_position
    ON form_fields (form_id, position);
CREATE INDEX IF NOT EXISTS idx_form_fields_form_id ON form_fields (form_id);
CREATE INDEX IF NOT EXISTS idx_form_fields_field_type ON form_fields (field_type);

-- One received submission.
CREATE TABLE IF NOT EXISTS form_submissions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    form_id UUID NOT NULL REFERENCES forms (id) ON DELETE CASCADE,
    status VARCHAR NOT NULL DEFAULT 'new'
        CHECK (status IN ('new', 'read', 'spam')),
    mapping_status VARCHAR NOT NULL DEFAULT 'none'
        CHECK (mapping_status IN ('none', 'mapped', 'failed')),
    mapping_error TEXT,
    mapped_target_id VARCHAR,
    ip_address VARCHAR(45),
    user_agent VARCHAR(512),
    submitted_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_form_submissions_form_id ON form_submissions (form_id);
CREATE INDEX IF NOT EXISTS idx_form_submissions_status ON form_submissions (status);
CREATE INDEX IF NOT EXISTS idx_form_submissions_mapping_status ON form_submissions (mapping_status);
CREATE INDEX IF NOT EXISTS idx_form_submissions_created_at ON form_submissions (created_at);

-- One submitted value.
CREATE TABLE IF NOT EXISTS form_submission_values (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    submission_id UUID NOT NULL REFERENCES form_submissions (id) ON DELETE CASCADE,
    field_id UUID NOT NULL REFERENCES form_fields (id) ON DELETE CASCADE,
    field_key VARCHAR(64) NOT NULL,
    value JSONB NOT NULL DEFAULT 'null'::JSONB,
    file_storage_key VARCHAR(512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_form_submission_values_submission_field
    ON form_submission_values (submission_id, field_id);
CREATE INDEX IF NOT EXISTS idx_form_submission_values_submission_id
    ON form_submission_values (submission_id);
CREATE INDEX IF NOT EXISTS idx_form_submission_values_field_id
    ON form_submission_values (field_id);
