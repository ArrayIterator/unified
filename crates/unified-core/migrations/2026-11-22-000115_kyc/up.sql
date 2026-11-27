-- KYC module (`src/modules/kyc/`): identity verification
-- cases with linked identity documents (references only),
-- AI verification verdicts, an admin review queue, risk
-- levels as data, verification contexts (the default context
-- the AI verification pipeline runs under) plus versioned
-- extra instructions, subject and document types as data,
-- per-tenant settings, and a hash-chained audit trail.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000114_sign: the
-- in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz. `user_id`
--     / `subject_id` / `submitted_by` / `actor_id` are plain TEXT
--     with NO foreign key on purpose, matching the rental
--     convention: users live in the users module and this module
--     must not assume its table or migration order.
--     `document_id` references a documents-module document by id
--     only, with NO foreign key: documents live in the documents
--     module. `case_id` / `context_id` / `risk_level_id`
--     reference KYC rows by id with NO foreign key, so deleting
--     one KYC record never orphans another module's rows.
--   - `tenant_id` is plain TEXT on every row: KYC data is
--     tenant business data and every query filters by it. A row
--     from another tenant reads as 404, never 403.
--   - Document bytes are never stored: `kyc_documents` holds
--     only the documents-module reference (`document_id`), the
--     SHA-256 of the document bytes (`content_hash`), and file
--     metadata (name, MIME type). The raw file never reaches
--     this module.
--   - The AI verdict is stored as JSON on the case
--     (`ai_verdict`: outcome, confidence, reasons, failed
--     checks, model, run time, and the context / instruction
--     versions that produced it), so a verdict is always
--     traceable to the exact default context and extra
--     instructions that were active when it ran.
--   - Nothing is hard-coded: document types, subject types,
--     risk levels, verification contexts, and extra
--     instructions are all rows, created and edited by an admin
--     through the API. This migration seeds no rows: a tenant
--     with no active verification context cannot run AI
--     verification (the endpoint answers 400), and a tenant
--     with no risk levels gets no risk level on its cases.
--     Only numeric / boolean knobs have defaults, in
--     `kyc_settings` below.
--   - The AI never auto-approves a case on its own: approval
--     is a human admin decision, except when an admin-enabled
--     policy row (`kyc_settings.auto_approve_low_risk`) says
--     otherwise for the configured low-risk level, and that
--     policy defaults to off.
--   - There is no floating point money in this module: KYC
--     carries no money fields.
--   - The audit trail is hash-chained: each entry stores the
--     SHA-256 of the previous entry hash plus its own payload
--     (`prev_hash|id|case_id|action|actor|created_at|detail`),
--     so tampering with any entry breaks the chain. The chain is
--     verified by recomputing every hash from the genesis hash
--     (64 zeroes).
--   - Unique indexes enforce the module invariants in the
--     database too: one document-type code, one subject-type
--     code, one risk-level code, and one verification-context
--     name per tenant; one active verification context per
--     tenant is enforced by the module, not by a database
--     constraint, because "unique among active rows" cannot be
--     expressed as a unique index; one document reference per
--     case. Risk-level score ranges must not overlap among
--     active levels for a tenant - also enforced by the module.
--   - `kyc_cases.status` is `draft`, `submitted`, `verifying`,
--     `more_info_requested`, `approved`, `rejected`, or
--     `expired`; `kyc_cases.subject_type` names a
--     `kyc_subject_types` row by code (no foreign key);
--     `kyc_documents.document_type_code` names a
--     `kyc_document_types` row by code (no foreign key).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Cases: one identity verification case for one subject (a user
-- id or a company id, as plain TEXT). `subject_type` names a
-- subject-type row by code. `risk_score` is 0-100 (NULL until
-- the first AI verdict); `risk_level_id` / `risk_level_code`
-- name the resolved risk level (NULL when the tenant has no
-- level covering the score). `ai_verdict` is the last AI
-- verdict as JSON (NULL until the first verification run).
-- `escalated` marks a case an admin escalated for senior
-- review. `expires_at` bounds the case lifetime.
CREATE TABLE IF NOT EXISTS kyc_cases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    subject_id TEXT NOT NULL CHECK (char_length(subject_id) BETWEEN 1 AND 200),
    subject_type TEXT NOT NULL CHECK (char_length(subject_type) BETWEEN 1 AND 128),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted', 'verifying', 'more_info_requested', 'approved', 'rejected', 'expired')),
    risk_score BIGINT CHECK (risk_score IS NULL OR (risk_score BETWEEN 0 AND 100)),
    risk_level_id UUID,
    risk_level_code TEXT CHECK (risk_level_code IS NULL OR char_length(risk_level_code) BETWEEN 1 AND 128),
    ai_verdict JSONB,
    submitted_by TEXT NOT NULL CHECK (char_length(submitted_by) BETWEEN 1 AND 200),
    reviewed_by TEXT CHECK (reviewed_by IS NULL OR char_length(reviewed_by) BETWEEN 1 AND 200),
    decision_reason TEXT CHECK (decision_reason IS NULL OR char_length(decision_reason) BETWEEN 1 AND 2000),
    escalated BOOLEAN NOT NULL DEFAULT FALSE,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_kyc_cases_tenant_status ON kyc_cases (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_kyc_cases_tenant_subject ON kyc_cases (tenant_id, subject_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_kyc_cases_tenant_risk ON kyc_cases (tenant_id, risk_level_code, created_at DESC) WHERE risk_level_code IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_kyc_cases_tenant_expires ON kyc_cases (tenant_id, expires_at) WHERE status IN ('submitted', 'verifying', 'more_info_requested');

-- Documents: one linked identity document on a case. Only the
-- documents-module reference and the SHA-256 of the document
-- bytes are stored - never the file itself.
-- `document_type_code` names a document-type row by code.
CREATE TABLE IF NOT EXISTS kyc_documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    case_id UUID NOT NULL,
    document_type_code TEXT NOT NULL CHECK (char_length(document_type_code) BETWEEN 1 AND 128),
    document_id TEXT NOT NULL CHECK (char_length(document_id) BETWEEN 1 AND 200),
    content_hash TEXT NOT NULL CHECK (char_length(content_hash) = 64),
    file_name TEXT CHECK (file_name IS NULL OR char_length(file_name) BETWEEN 1 AND 255),
    mime_type TEXT CHECK (mime_type IS NULL OR char_length(mime_type) BETWEEN 1 AND 128),
    uploaded_by TEXT NOT NULL CHECK (char_length(uploaded_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_kyc_documents_case_document ON kyc_documents (case_id, document_id);
CREATE INDEX IF NOT EXISTS idx_kyc_documents_tenant_case ON kyc_documents (tenant_id, case_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_kyc_documents_tenant_type ON kyc_documents (tenant_id, document_type_code, created_at DESC);

-- Document types: the document kinds a tenant accepts, as data.
-- `subject_types` lists the subject-type codes the kind
-- applies to; an empty list means it applies to every subject
-- type. Inactive kinds cannot be linked to new cases.
CREATE TABLE IF NOT EXISTS kyc_document_types (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    subject_types JSONB NOT NULL DEFAULT '[]'::jsonb,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_kyc_document_types_tenant_code ON kyc_document_types (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_kyc_document_types_tenant_active ON kyc_document_types (tenant_id, active, created_at DESC);

-- Subject types: the subject kinds a tenant verifies, as data.
-- `required_document_type_codes` lists the document-type codes
-- a case of this kind must link before it can be submitted.
CREATE TABLE IF NOT EXISTS kyc_subject_types (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    required_document_type_codes JSONB NOT NULL DEFAULT '[]'::jsonb,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_kyc_subject_types_tenant_code ON kyc_subject_types (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_kyc_subject_types_tenant_active ON kyc_subject_types (tenant_id, active, created_at DESC);

-- Verification contexts: the DEFAULT CONTEXT the AI
-- verification pipeline runs under, as data. One context is
-- active per tenant (enforced by the module). `visible_fields`
-- names the case fields and document metadata the AI may see;
-- `checks` names the checks the AI must run (values are data -
-- the module never invents a check name). `version` increments
-- on every update; a verdict records the version it ran under.
CREATE TABLE IF NOT EXISTS kyc_verification_contexts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    active BOOLEAN NOT NULL DEFAULT FALSE,
    visible_fields JSONB NOT NULL DEFAULT '[]'::jsonb,
    checks JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_kyc_contexts_tenant_name ON kyc_verification_contexts (tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_kyc_contexts_tenant_active ON kyc_verification_contexts (tenant_id, active, version DESC);

-- Instructions: ADDITIONAL INSTRUCTIONS declared on top of a
-- verification context, as data. `context_id` NULL means the
-- instruction applies to every context of the tenant.
-- Instructions are versioned: an update creates a new row with
-- `version` + 1 and deactivates the old row, which keeps
-- `supersedes_id` pointing at its replacement, so the history
-- is never rewritten. `priority` orders instructions (lowest
-- first) when they are merged into the resolved context.
CREATE TABLE IF NOT EXISTS kyc_instructions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    context_id UUID,
    text TEXT NOT NULL CHECK (char_length(text) BETWEEN 1 AND 10000),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    priority BIGINT NOT NULL DEFAULT 0,
    supersedes_id UUID,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_kyc_instructions_tenant_context ON kyc_instructions (tenant_id, context_id, active, priority ASC, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_kyc_instructions_tenant_active ON kyc_instructions (tenant_id, active, created_at DESC);

-- Risk levels: the risk bands a tenant uses, as data. A case
-- score (0-100) resolves to the level whose [min_score,
-- max_score] contains it. Active levels of a tenant must not
-- overlap (enforced by the module).
CREATE TABLE IF NOT EXISTS kyc_risk_levels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    min_score BIGINT NOT NULL CHECK (min_score BETWEEN 0 AND 100),
    max_score BIGINT NOT NULL CHECK (max_score BETWEEN 0 AND 100),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_kyc_risk_levels_range CHECK (min_score <= max_score)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_kyc_risk_levels_tenant_code ON kyc_risk_levels (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_kyc_risk_levels_tenant_active ON kyc_risk_levels (tenant_id, active, min_score ASC);

-- Audit trail: one row per case event (created, submitted,
-- verified, approved, rejected, more_info_requested,
-- escalated, expired, documents_updated, verification_failed,
-- auto_approved, updated, deleted). `hash` is the SHA-256 of
-- `prev_hash|id|case_id|action|actor|created_at|detail`; the
-- first entry of a case chains from the genesis hash (64
-- zeroes).
CREATE TABLE IF NOT EXISTS kyc_audit_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    case_id UUID NOT NULL,
    action TEXT NOT NULL CHECK (char_length(action) BETWEEN 1 AND 128),
    actor_id TEXT NOT NULL CHECK (char_length(actor_id) BETWEEN 1 AND 200),
    detail TEXT CHECK (detail IS NULL OR char_length(detail) BETWEEN 1 AND 5000),
    prev_hash TEXT NOT NULL CHECK (char_length(prev_hash) = 64),
    hash TEXT NOT NULL CHECK (char_length(hash) = 64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_kyc_audit_tenant_case ON kyc_audit_entries (tenant_id, case_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_kyc_audit_tenant_action ON kyc_audit_entries (tenant_id, action, created_at DESC);

-- Per-tenant KYC settings. `auto_approve_low_risk` is the only
-- path by which an AI verdict can approve a case without a
-- human, and only for the configured low-risk level code; it
-- defaults to off. `require_ai_verification` gates the verify
-- endpoint. `verification_expiry_days` bounds new cases.
CREATE TABLE IF NOT EXISTS kyc_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    auto_approve_low_risk BOOLEAN NOT NULL DEFAULT FALSE,
    auto_approve_risk_level_code TEXT NOT NULL DEFAULT 'low' CHECK (char_length(auto_approve_risk_level_code) BETWEEN 1 AND 128),
    require_ai_verification BOOLEAN NOT NULL DEFAULT TRUE,
    verification_expiry_days BIGINT NOT NULL DEFAULT 30 CHECK (verification_expiry_days >= 1),
    max_documents_per_case BIGINT NOT NULL DEFAULT 10 CHECK (max_documents_per_case >= 1),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
