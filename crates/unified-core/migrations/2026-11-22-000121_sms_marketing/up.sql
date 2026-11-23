-- SMS marketing module (`src/modules/sms_marketing/`):
-- SMS marketing: admin-configurable SMS providers as
-- data (a generic HTTP adapter posting JSON to the
-- configured endpoint with the configured auth, plus a
-- log-only test provider; credentials write-only and
-- encrypted, webhook secrets hash-stored and returned
-- once), plain-text templates with {{variable}}
-- substitution and GSM-7 / UCS-2 segment counting,
-- contact lists with tags and per-contact variables,
-- campaigns with a draft / scheduled / sending / sent /
-- paused / cancelled status machine and audience
-- selection by list reference plus data segment filters
-- plus explicit numbers, a send pass that blocks opted-out
-- numbers and enforces per-tenant and per-provider
-- per-minute limits at enqueue time (over-limit messages
-- stay queued, never dropped), per-message delivery
-- tracking with a shared-secret delivery webhook and an
-- inbound keyword webhook, opt-out keywords as data with a
-- per-tenant opt-out list, and an append-only audit trail.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000120_referral:
-- the in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz.
--     `template_id` / `provider_id` / `list_id` /
--     `campaign_id` reference SMS marketing rows by id with NO
--     foreign key, so this module must not assume any other
--     module's tables or migration order. `created_by` /
--     `updated_by` / `actor` are plain TEXT with NO foreign key
--     on purpose: users live in the users module and this module
--     must not assume its table or migration order.
--   - `tenant_id` is plain TEXT on every row: SMS data is
--     tenant business data and every query filters by it.
--     A row from another tenant reads as 404, never 403.
--   - Nothing is hard-coded: providers (including their
--     endpoint, auth type, sender id, per-provider rate
--     limit, and enabled flag), templates, contact lists,
--     contacts (including their tags and variables),
--     campaigns (including their audience lists, explicit
--     numbers, and segment filter), opt-out keywords, and
--     the per-tenant rate limit are all data rows. The only
--     fixed vocabulary is the provider kind (`http`,
--     `log_only`), the auth type (`none`, `bearer`,
--     `api_key_header`, `basic`), the campaign status
--     (`draft`, `scheduled`, `sending`, `sent`, `paused`,
--     `cancelled`), the message status (`queued`, `sent`,
--     `delivered`, `failed`, `undelivered`, `skipped`), the
--     keyword kind (`opt_out`, `opt_in`), and the segment
--     encoding (`gsm7`, `ucs2`) — those are the shape of
--     the module, not its content.
--   - No money is stored anywhere in this module: no price,
--     cost, or charge column exists, and `f64` is never used
--     for a stored or returned number.
--   - Provider credentials are write-only: `credential_encrypted`
--     holds only the `v1.` HMAC-keystream ciphertext (the same
--     format as the holidays module), `credential_fingerprint`
--     holds only the first 12 hex characters of the SHA-256 of
--     the credential, and the raw credential is never stored.
--     `webhook_secret_hash` holds only the SHA-256 hex of the
--     delivery / inbound webhook secret; the raw secret is
--     returned exactly once at creation / rotation and never
--     stored. No real provider name or credential is seeded
--     by this migration.
--   - Recipient phone numbers are stored normalized to E.164
--     (`+` plus 8-15 digits). Message bodies are the rendered
--     text stored at send time, so delivery reports stay
--     readable after the template changes. `segments` and
--     `encoding` are the GSM-7 / UCS-2 segment math computed
--     at send time. `provider_message_id` is the id the
--     provider returned, when it returned one; `error` holds
--     the provider's failure text truncated to 500
--     characters, never a credential.
--   - The opt-out list is the single source of truth for
--     blocking sends: one row per (tenant, phone number).
--     Opt-out keyword rows are data (the conventional
--     defaults are seeded by the application on first read,
--     not by this migration), and matching always reads the
--     rows. Removing an opt-out row re-opts the number back
--     in. `user.deleted` anonymizes identity fields only:
--     no row here links a user id to a phone number, so
--     recipient numbers are never redacted by deletion.
--   - Rate limits are enforced at enqueue time, never by
--     dropping: a message over the per-tenant or
--     per-provider per-minute limit stays `queued` with
--     `queued_reason` `rate_limited` until it is retried.
--   - Deleting a provider, a template, a contact list, or a
--     campaign never deletes the rows that reference it:
--     messages keep their provider and campaign snapshots,
--     campaigns keep their template and provider ids, and
--     audit rows stay on record. Audit rows are likewise
--     never updated or deleted through the API.
--   - Unique indexes enforce the module invariants in the
--     database too: one opt-out per (tenant, phone number),
--     one keyword per (tenant, keyword, kind), one contact
--     per (list, phone number), one rate-limit row per
--     tenant, and one provider name per tenant.
--   - This migration seeds no rows: a tenant with no
--     provider rows cannot send yet, a tenant with no
--     keyword rows has no keyword handling until it adds
--     some, and a tenant with no rate-limit row uses the
--     application default of 60 sends per minute.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- SMS providers: one admin-configured sending provider per
-- tenant. `kind` is `http` (generic adapter: POST JSON to
-- `endpoint_url` with the configured auth) or `log_only`
-- (test provider: no endpoint, no network call).
-- `auth_type` is `none`, `bearer`, `api_key_header`, or
-- `basic`; `auth_header` names the header for api-key
-- auth, `auth_username` is the basic-auth username.
-- `credential_encrypted` holds only the `v1.` ciphertext;
-- `credential_fingerprint` holds only the first 12 hex
-- characters of the credential's SHA-256.
-- `webhook_secret_hash` holds only the SHA-256 hex of the
-- delivery / inbound webhook secret. `sender_id` is the
-- sender id sent as `from`. `sends_per_minute` is the
-- per-provider rate limit. `request_fields` is an optional
-- JSON object of extra fields merged into every HTTP
-- adapter request body.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_providers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    kind TEXT NOT NULL CHECK (kind IN ('http', 'log_only')),
    endpoint_url TEXT CHECK (endpoint_url IS NULL OR char_length(endpoint_url) BETWEEN 1 AND 2000),
    auth_type TEXT NOT NULL DEFAULT 'none' CHECK (auth_type IN ('none', 'bearer', 'api_key_header', 'basic')),
    auth_header TEXT CHECK (auth_header IS NULL OR char_length(auth_header) BETWEEN 1 AND 200),
    auth_username TEXT CHECK (auth_username IS NULL OR char_length(auth_username) BETWEEN 1 AND 200),
    credential_encrypted TEXT,
    credential_fingerprint TEXT CHECK (credential_fingerprint IS NULL OR char_length(credential_fingerprint) = 12),
    webhook_secret_hash TEXT CHECK (webhook_secret_hash IS NULL OR char_length(webhook_secret_hash) = 64),
    sender_id TEXT NOT NULL CHECK (char_length(sender_id) BETWEEN 1 AND 20),
    sends_per_minute BIGINT NOT NULL DEFAULT 60 CHECK (sends_per_minute BETWEEN 0 AND 100000),
    request_fields JSONB NOT NULL DEFAULT 'null'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind <> 'http' OR endpoint_url IS NOT NULL),
    CHECK (kind <> 'log_only' OR endpoint_url IS NULL)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sms_providers_tenant_name ON sms_providers (tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_sms_providers_tenant_kind ON sms_providers (tenant_id, kind, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sms_providers_tenant_enabled ON sms_providers (tenant_id, enabled, created_at DESC);

-- ---------------------------------------------------------------------------
-- SMS templates: one plain-text template per tenant. `body`
-- holds `{{variable}}` placeholders; `variables` lists the
-- placeholder names derived from the body at write time.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    body TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 5000),
    variables JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sms_templates_tenant_created ON sms_templates (tenant_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Contact lists: one named audience per tenant. Members live
-- in `sms_contacts`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_contact_lists (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sms_contact_lists_tenant_created ON sms_contact_lists (tenant_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Contacts: one member of a contact list. `phone_number` is
-- the normalized E.164 number; `tags` drive segment
-- filters and `variables` drive template substitution.
-- One number per list.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_contacts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    list_id UUID NOT NULL,
    phone_number TEXT NOT NULL CHECK (char_length(phone_number) BETWEEN 1 AND 20),
    display_name TEXT CHECK (display_name IS NULL OR char_length(display_name) BETWEEN 1 AND 200),
    tags JSONB NOT NULL DEFAULT '[]'::jsonb,
    variables JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sms_contacts_list_number ON sms_contacts (list_id, phone_number);
CREATE INDEX IF NOT EXISTS idx_sms_contacts_tenant_list ON sms_contacts (tenant_id, list_id, created_at ASC);

-- ---------------------------------------------------------------------------
-- Campaigns: one SMS campaign per tenant. `template_id` and
-- `provider_id` are plain references (no foreign key).
-- `contact_list_ids` is a JSON array of list ids by
-- reference; `recipient_numbers` is a JSON array of
-- explicit E.164 numbers; `segment_filter` is the JSON
-- object evaluated against contact tags and variables
-- (`tags_all`, `tags_any`, `variables`). `variables` holds
-- campaign-level template defaults (per-contact variables
-- win). `status` is `draft`, `scheduled`, `sending`,
-- `sent`, `paused`, or `cancelled`; `send_at` is the
-- scheduled send time and `sent_at` is when the send pass
-- ran. `stats_*` are the send counts kept on the campaign
-- row.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    template_id UUID NOT NULL,
    provider_id UUID NOT NULL,
    contact_list_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    recipient_numbers JSONB NOT NULL DEFAULT '[]'::jsonb,
    segment_filter JSONB NOT NULL DEFAULT '{}'::jsonb,
    variables JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'scheduled', 'sending', 'sent', 'paused', 'cancelled')),
    send_at TIMESTAMPTZ,
    sent_at TIMESTAMPTZ,
    stats_queued BIGINT NOT NULL DEFAULT 0 CHECK (stats_queued >= 0),
    stats_sent BIGINT NOT NULL DEFAULT 0 CHECK (stats_sent >= 0),
    stats_delivered BIGINT NOT NULL DEFAULT 0 CHECK (stats_delivered >= 0),
    stats_failed BIGINT NOT NULL DEFAULT 0 CHECK (stats_failed >= 0),
    stats_skipped BIGINT NOT NULL DEFAULT 0 CHECK (stats_skipped >= 0),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sms_campaigns_tenant_status ON sms_campaigns (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sms_campaigns_tenant_provider ON sms_campaigns (tenant_id, provider_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sms_campaigns_tenant_template ON sms_campaigns (tenant_id, template_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Messages: one outbound SMS per recipient of a campaign.
-- `body` is the rendered text stored at send time.
-- `segments` and `encoding` are the GSM-7 / UCS-2 segment
-- math computed at send time. `status` is `queued`,
-- `sent`, `delivered`, `failed`, `undelivered`, or
-- `skipped`; `queued_reason` explains a queued (`rate_limited`)
-- or skipped (`opted_out`, `missing_variable`) row;
-- `provider_message_id` is the id the provider returned,
-- when it returned one; `error` is the provider's failure
-- text truncated to 500 characters, never a credential.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    campaign_id UUID NOT NULL,
    provider_id UUID NOT NULL,
    phone_number TEXT NOT NULL CHECK (char_length(phone_number) BETWEEN 1 AND 20),
    body TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 5000),
    segments BIGINT NOT NULL DEFAULT 0 CHECK (segments >= 0),
    encoding TEXT NOT NULL DEFAULT 'gsm7' CHECK (encoding IN ('gsm7', 'ucs2')),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'sent', 'delivered', 'failed', 'undelivered', 'skipped')),
    queued_reason TEXT CHECK (queued_reason IS NULL OR char_length(queued_reason) BETWEEN 1 AND 64),
    provider_message_id TEXT CHECK (provider_message_id IS NULL OR char_length(provider_message_id) BETWEEN 1 AND 200),
    error TEXT CHECK (error IS NULL OR char_length(error) BETWEEN 1 AND 500),
    sent_at TIMESTAMPTZ,
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sms_messages_tenant_campaign ON sms_messages (tenant_id, campaign_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_sms_messages_tenant_provider ON sms_messages (tenant_id, provider_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sms_messages_tenant_status ON sms_messages (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sms_messages_provider_message ON sms_messages (provider_message_id);

-- ---------------------------------------------------------------------------
-- Opt-outs: one opted-out number per tenant. Sends to a
-- listed number are blocked at send time. `keyword` is the
-- matched inbound keyword, or `manual` / `api` for rows
-- added by hand; `source` names how the row arrived
-- (`keyword`, `manual`, `api`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_opt_outs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    phone_number TEXT NOT NULL CHECK (char_length(phone_number) BETWEEN 1 AND 20),
    keyword TEXT NOT NULL CHECK (char_length(keyword) BETWEEN 1 AND 32),
    source TEXT NOT NULL CHECK (source IN ('keyword', 'manual', 'api')),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sms_opt_outs_tenant_number ON sms_opt_outs (tenant_id, phone_number);
CREATE INDEX IF NOT EXISTS idx_sms_opt_outs_tenant_created ON sms_opt_outs (tenant_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Opt-out keywords: one keyword row per tenant. `kind` is
-- `opt_out` (adds the sender to the opt-out list) or
-- `opt_in` (removes them). Matching reads these rows, so a
-- tenant with no rows has no keyword handling until it adds
-- (or the application seeds) some.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_opt_out_keywords (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    keyword TEXT NOT NULL CHECK (char_length(keyword) BETWEEN 1 AND 32),
    kind TEXT NOT NULL CHECK (kind IN ('opt_out', 'opt_in')),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sms_keywords_tenant_keyword_kind ON sms_opt_out_keywords (tenant_id, keyword, kind);
CREATE INDEX IF NOT EXISTS idx_sms_keywords_tenant_kind ON sms_opt_out_keywords (tenant_id, kind, created_at ASC);

-- ---------------------------------------------------------------------------
-- Rate limits: one per-tenant send rate limit (sends per
-- minute for the whole tenant), enforced at enqueue time
-- alongside the per-provider limit. A tenant with no row
-- uses the application default of 60.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_rate_limits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    sends_per_minute BIGINT NOT NULL CHECK (sends_per_minute BETWEEN 0 AND 100000),
    updated_by TEXT NOT NULL CHECK (char_length(updated_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sms_rate_limits_tenant ON sms_rate_limits (tenant_id);

-- ---------------------------------------------------------------------------
-- Audit trail: one row per change to a provider, a template,
-- a contact list, a contact, a campaign, a message, an
-- opt-out, a keyword, or a rate limit (creates, updates,
-- deletes, schedules, pauses, cancels, sends, retries,
-- webhook-secret rotations). `entity_type` names the row
-- kind; `entity_id` is the row id as TEXT; `action` names
-- the change; `before` / `after` are the serialized rows
-- (or NULL for creates / deletes). Audit rows are never
-- updated or deleted through the API.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sms_audit (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    entity_type TEXT NOT NULL CHECK (char_length(entity_type) BETWEEN 1 AND 64),
    entity_id TEXT NOT NULL CHECK (char_length(entity_id) BETWEEN 1 AND 200),
    action TEXT NOT NULL CHECK (char_length(action) BETWEEN 1 AND 64),
    actor TEXT NOT NULL CHECK (char_length(actor) BETWEEN 1 AND 200),
    before JSONB,
    after JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sms_audit_tenant_created ON sms_audit (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sms_audit_tenant_entity ON sms_audit (tenant_id, entity_type, entity_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sms_audit_tenant_action ON sms_audit (tenant_id, action, created_at DESC);
