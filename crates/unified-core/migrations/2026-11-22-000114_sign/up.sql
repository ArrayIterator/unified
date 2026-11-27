-- Sign module (`src/modules/sign/`): core e-signature -
-- envelopes (signing requests) over multiple documents,
-- signers with hashed signing tokens, signature capture with
-- tamper evidence, reusable templates, data-driven reminder
-- schedules, hash-chained audit entries, and completion
-- certificates. PostgreSQL only. Run with
-- diesel_migrations (embedded via crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000113_plm: the
-- in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz. `user_id`
--     / `created_by` / `actor_id` are plain TEXT with NO foreign key
--     on purpose, matching the rental convention: users live in the
--     users module and this module must not assume its table or
--     migration order. `document_ids` are likewise references by id
--     only, with NO foreign key: documents live in the documents
--     module. `template_id` references a sign template by id, with
--     NO foreign key, so a deleted template never orphans an
--     envelope built from it.
--   - `tenant_id` is plain TEXT on every row: sign data is
--     tenant business data and every query filters by it. A row
--     from another tenant reads as 404, never 403.
--   - Signing tokens are never stored: `sign_signers` holds only
--     `token_hash` (the SHA-256 of the token) and its expiry. The
--     raw token is returned once at send / resend / add and is
--     never persisted. A blank `token_hash` means no live token
--     (draft, signed, declined, or revoked).
--   - Signature bytes are never stored: `sign_signers.signature`
--     holds only hashes - the SHA-256 of the signature bytes
--     (drawn strokes, rendered typed name, or uploaded image),
--     the SHA-256 of the document as the signer saw it, and
--     hashes of the IP and user agent. The raw values never reach
--     the module.
--   - Signing order and reminder schedules are data, not code:
--     `sign_envelopes.signing_order` is `sequential` or
--     `parallel`; `reminder_schedule` is a JSON object naming
--     `interval_days`, `max_reminders` (null is unlimited), and
--     `start_after_days`. Signature types are data too:
--     `sign_settings.signature_types` names the capture methods
--     the tenant allows (a subset of drawn / typed / uploaded),
--     and `provider` names the signing provider integration to
--     use, with its configuration in `provider_settings`.
--   - There is no floating point money in this module: sign
--     carries no money fields.
--   - The audit trail is hash-chained: each entry stores the
--     SHA-256 of the previous entry hash plus its own payload
--     (`prev_hash|id|envelope_id|action|actor|created_at|detail`),
--     so tampering with any entry breaks the chain. The chain is
--     verified by recomputing every hash from the genesis hash
--     (64 zeroes).
--   - Unique indexes enforce the module invariants in the
--     database too: one template name per tenant; one signer
--     email per envelope (case-insensitive); one certificate
--     hash overall. One signer order index per envelope and
--     strictly increasing reminder counts are enforced by the
--     module, not by a database constraint, because "unique
--     among live rows" and "greater than the current maximum"
--     cannot be expressed as a unique index.
--   - `sign_envelopes.status` is `draft`, `sent`,
--     `partially_signed`, `completed`, `declined`, `expired`,
--     `cancelled`, or `voided`; `sign_signers.status` is
--     `pending`, `sent`, `viewed`, `signed`, or `declined`;
--     `sign_signers.signature` (JSON) records `method` as
--     `drawn`, `typed`, or `uploaded`.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Envelopes: one signing request. `document_ids` references
-- documents-module documents by id only; `document_hashes` holds
-- the SHA-256 of each document as recorded at send time, aligned
-- with `document_ids` by position. `signing_order` is
-- `sequential` or `parallel`. `template_id` records the template
-- the envelope was built from, when there is one.
-- `certificate_hash` is set when the envelope completes.
CREATE TABLE IF NOT EXISTS sign_envelopes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    message TEXT CHECK (message IS NULL OR char_length(message) BETWEEN 1 AND 5000),
    document_ids TEXT[] NOT NULL DEFAULT '{}',
    document_hashes TEXT[] NOT NULL DEFAULT '{}',
    signing_order TEXT NOT NULL DEFAULT 'sequential' CHECK (signing_order IN ('sequential', 'parallel')),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'sent', 'partially_signed', 'completed', 'declined', 'expired', 'cancelled', 'voided')),
    template_id UUID,
    expires_at TIMESTAMPTZ,
    sent_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    certificate_hash TEXT,
    reminder_schedule JSONB NOT NULL DEFAULT '{"interval_days": 7, "max_reminders": null, "start_after_days": 3}'::jsonb,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_sign_envelopes_hashes_align CHECK (cardinality(document_hashes) = cardinality(document_ids)),
    CONSTRAINT chk_sign_envelopes_completed_fields CHECK ((status = 'completed' AND completed_at IS NOT NULL AND certificate_hash IS NOT NULL) OR (status <> 'completed'))
);
CREATE INDEX IF NOT EXISTS idx_sign_envelopes_tenant_status ON sign_envelopes (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sign_envelopes_tenant_template ON sign_envelopes (tenant_id, template_id) WHERE template_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_sign_envelopes_certificate ON sign_envelopes (certificate_hash) WHERE certificate_hash IS NOT NULL;

-- Signers: one signer on an envelope. `token_hash` is the SHA-256
-- of the current signing token (never the raw token);
-- `token_expires_at` bounds it. `signature` is the capture
-- evidence as JSON: `method`, `signature_hash`,
-- `document_hash`, `typed_name`, `ip_hash`,
-- `user_agent_hash`, and `signed_at` - hashes only, never raw
-- signature bytes or raw IP / user-agent values.
CREATE TABLE IF NOT EXISTS sign_signers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    envelope_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    email TEXT NOT NULL CHECK (char_length(email) BETWEEN 1 AND 254),
    role TEXT NOT NULL DEFAULT 'signer' CHECK (char_length(role) BETWEEN 1 AND 128),
    order_index BIGINT NOT NULL DEFAULT 0 CHECK (order_index >= 0),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'viewed', 'signed', 'declined')),
    token_hash TEXT NOT NULL DEFAULT '',
    token_expires_at TIMESTAMPTZ,
    viewed_at TIMESTAMPTZ,
    signed_at TIMESTAMPTZ,
    declined_at TIMESTAMPTZ,
    decline_reason TEXT CHECK (decline_reason IS NULL OR char_length(decline_reason) BETWEEN 1 AND 2000),
    signature JSONB,
    reminders_sent BIGINT NOT NULL DEFAULT 0 CHECK (reminders_sent >= 0),
    last_reminder_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_sign_signers_signed_fields CHECK ((status = 'signed' AND signed_at IS NOT NULL AND signature IS NOT NULL) OR (status <> 'signed')),
    CONSTRAINT chk_sign_signers_declined_fields CHECK ((status = 'declined' AND declined_at IS NOT NULL) OR (status <> 'declined'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sign_signers_envelope_email ON sign_signers (envelope_id, lower(email));
CREATE INDEX IF NOT EXISTS idx_sign_signers_envelope_order ON sign_signers (envelope_id, order_index);
CREATE INDEX IF NOT EXISTS idx_sign_signers_tenant_status ON sign_signers (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sign_signers_token_hash ON sign_signers (token_hash) WHERE token_hash <> '';

-- Templates: one reusable signing template. `signer_roles` is the
-- ordered list of signer roles (each with optional default name /
-- email) and `fields` the template fields, both as JSON: they are
-- data, and an envelope built from the template snapshots them.
CREATE TABLE IF NOT EXISTS sign_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    document_ids TEXT[] NOT NULL DEFAULT '{}',
    signer_roles JSONB NOT NULL DEFAULT '[]'::jsonb,
    fields JSONB NOT NULL DEFAULT '[]'::jsonb,
    signing_order TEXT NOT NULL DEFAULT 'sequential' CHECK (signing_order IN ('sequential', 'parallel')),
    reminder_schedule JSONB NOT NULL DEFAULT '{"interval_days": 7, "max_reminders": null, "start_after_days": 3}'::jsonb,
    expires_in_days BIGINT CHECK (expires_in_days IS NULL OR expires_in_days >= 1),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sign_templates_tenant_name ON sign_templates (tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_sign_templates_tenant_created ON sign_templates (tenant_id, created_at DESC);

-- Audit trail: one row per signing event (created, sent, viewed,
-- signed, declined, resent, reminded, updated, cancelled,
-- voided, expired, completed, signer added / updated / removed).
-- `hash` is the SHA-256 of
-- `prev_hash|id|envelope_id|action|actor|created_at|detail`; the
-- first entry of an envelope chains from the genesis hash (64
-- zeroes).
CREATE TABLE IF NOT EXISTS sign_audit_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    envelope_id UUID NOT NULL,
    signer_id UUID,
    action TEXT NOT NULL CHECK (char_length(action) BETWEEN 1 AND 128),
    actor_id TEXT NOT NULL CHECK (char_length(actor_id) BETWEEN 1 AND 200),
    detail TEXT CHECK (detail IS NULL OR char_length(detail) BETWEEN 1 AND 5000),
    prev_hash TEXT NOT NULL CHECK (char_length(prev_hash) = 64),
    hash TEXT NOT NULL CHECK (char_length(hash) = 64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sign_audit_tenant_envelope ON sign_audit_entries (tenant_id, envelope_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_sign_audit_tenant_action ON sign_audit_entries (tenant_id, action, created_at DESC);

-- Certificates: one completion certificate per completed
-- envelope. `certificate_hash` is the SHA-256 of the envelope
-- id, document hashes, signer evidence, and completion time;
-- `audit_head_hash` is the hash of the last audit entry, so the
-- certificate is bound to the trail. `signers` is the signer
-- evidence as JSON.
CREATE TABLE IF NOT EXISTS sign_certificates (
    certificate_hash TEXT PRIMARY KEY CHECK (char_length(certificate_hash) = 64),
    envelope_id UUID NOT NULL,
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    document_ids TEXT[] NOT NULL DEFAULT '{}',
    document_hashes TEXT[] NOT NULL DEFAULT '{}',
    signers JSONB NOT NULL DEFAULT '[]'::jsonb,
    audit_head_hash TEXT NOT NULL CHECK (char_length(audit_head_hash) = 64),
    audit_event_count BIGINT NOT NULL DEFAULT 0 CHECK (audit_event_count >= 0),
    completed_at TIMESTAMPTZ NOT NULL,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sign_certificates_envelope ON sign_certificates (envelope_id);
CREATE INDEX IF NOT EXISTS idx_sign_certificates_tenant ON sign_certificates (tenant_id, completed_at DESC);

-- Per-tenant sign settings. `signature_types` names the capture
-- methods the tenant allows; `provider` names the signing
-- provider integration to use, with its configuration in
-- `provider_settings`. `default_reminder_schedule` is the
-- schedule envelopes inherit when the caller sets none.
CREATE TABLE IF NOT EXISTS sign_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    signature_types JSONB NOT NULL DEFAULT '["drawn", "typed", "uploaded"]'::jsonb,
    default_expiry_days BIGINT NOT NULL DEFAULT 30 CHECK (default_expiry_days >= 1),
    default_reminder_schedule JSONB NOT NULL DEFAULT '{"interval_days": 7, "max_reminders": null, "start_after_days": 3}'::jsonb,
    reminder_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    require_view_before_sign BOOLEAN NOT NULL DEFAULT FALSE,
    provider TEXT NOT NULL DEFAULT 'internal' CHECK (char_length(provider) BETWEEN 1 AND 128),
    provider_settings JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
