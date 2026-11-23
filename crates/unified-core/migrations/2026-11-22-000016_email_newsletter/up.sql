-- Email tables for the Unified Pentagonal Project
-- (email_providers.rs, mail.rs, email_templates.rs, newsletter.rs):
-- provider connections and their send log, the SMTP settings and
-- mail log, email templates with versions, and newsletter lists,
-- subscribers, campaigns, and tracking events.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `email_connections.kind` is `mailchimp`, `brevo`,
--     `sendgrid`, `mailgun`, `postmark`, `ses`, `resend`,
--     `sparkpost`, or `smtp` (nine providers).
--   - `email_connections.credentials` stores the credential
--     document as JSONB. The values are write-only: they are never
--     serialized into a view, never logged, and never included in
--     hook payloads. They must be encrypted before insert, the same
--     reservation as the `*_encrypted` columns of
--     `vcs_provider_settings` (no encryption helper exists yet).
--   - `mail_smtp_settings` is a singleton row keyed by `key =
--     'global'`. The SMTP password is write-only and is stored in
--     `password_encrypted`: encrypt before insert, same reservation
--     as above. `connector` is `smtp`, `local`, or `log`.
--   - `email_templates.versions` from the `EmailTemplate` struct
--     is normalized into `email_template_versions` instead of
--     embedded JSON, the same normalization the store module uses
--     for order items. The current version's subject / html / json
--     stay denormalized on the template row.
--   - `newsletter_campaigns.template_id` references
--     `email_templates.id` (GrapeJS template ids come from the
--     Email Templates module).
--   - `newsletter_subscribers.status` is `pending`, `confirmed`,
--     `unsubscribed`, `bounced`, or `complained`.
--     `newsletter_campaigns.status` is `draft`, `scheduled`,
--     `sending`, `sent`, `completed`, or `failed`.
--   - The newsletter module keeps timestamps as RFC 3339 strings
--     in code; they are TIMESTAMPTZ here, matching every other
--     table in this project.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One stored email provider connection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS email_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('mailchimp', 'brevo', 'sendgrid', 'mailgun', 'postmark', 'ses', 'resend', 'sparkpost', 'smtp')),
    domain VARCHAR,
    region VARCHAR,
    sender_email VARCHAR,
    sender_name VARCHAR,
    webhook_url VARCHAR,
    credentials JSONB NOT NULL DEFAULT '{}'::jsonb,
    test_mode BOOLEAN NOT NULL DEFAULT FALSE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_tested_at TIMESTAMPTZ,
    last_status VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_email_connections_name
    ON email_connections (name);
CREATE INDEX IF NOT EXISTS idx_email_connections_kind
    ON email_connections (kind);
CREATE INDEX IF NOT EXISTS idx_email_connections_enabled
    ON email_connections (enabled);

-- ---------------------------------------------------------------------------
-- One entry of the provider send log.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS email_send_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id UUID REFERENCES email_connections (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    provider VARCHAR NOT NULL,
    "to" VARCHAR NOT NULL,
    subject VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    provider_message_id VARCHAR,
    test_mode BOOLEAN NOT NULL DEFAULT FALSE,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_email_send_log_connection_id
    ON email_send_log (connection_id);
CREATE INDEX IF NOT EXISTS idx_email_send_log_status
    ON email_send_log (status);
CREATE INDEX IF NOT EXISTS idx_email_send_log_created_at
    ON email_send_log (created_at);

-- ---------------------------------------------------------------------------
-- SMTP settings (singleton row).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS mail_smtp_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    host VARCHAR NOT NULL DEFAULT '',
    port INTEGER NOT NULL DEFAULT 587 CHECK (port BETWEEN 1 AND 65535),
    tls BOOLEAN NOT NULL DEFAULT TRUE,
    username VARCHAR NOT NULL DEFAULT '',
    password_encrypted TEXT NOT NULL DEFAULT '',
    sender_name VARCHAR NOT NULL DEFAULT '',
    sender_email VARCHAR NOT NULL DEFAULT '',
    connector VARCHAR NOT NULL DEFAULT 'smtp' CHECK (connector IN ('smtp', 'local', 'log')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- One entry of the mail send log. `status` is `queued`, `sent`, or
-- `failed`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS mail_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    "to" VARCHAR NOT NULL,
    subject VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('queued', 'sent', 'failed')),
    connector VARCHAR NOT NULL,
    -- References email_templates; the foreign key is added by
    -- ALTER TABLE at the end of this file, after that table exists.
    template_id UUID,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_mail_log_status
    ON mail_log (status);
CREATE INDEX IF NOT EXISTS idx_mail_log_created_at
    ON mail_log (created_at);

-- ---------------------------------------------------------------------------
-- One email template (GrapeJS design + subject + category).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS email_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    subject VARCHAR NOT NULL,
    category VARCHAR NOT NULL DEFAULT '',
    html TEXT NOT NULL DEFAULT '',
    json JSONB NOT NULL DEFAULT '{}'::jsonb,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_email_templates_category
    ON email_templates (category);

-- ---------------------------------------------------------------------------
-- One stored version of a template.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS email_template_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES email_templates (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    version INTEGER NOT NULL CHECK (version >= 1),
    subject VARCHAR NOT NULL,
    html TEXT NOT NULL DEFAULT '',
    json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (template_id, version)
);

CREATE INDEX IF NOT EXISTS idx_email_template_versions_template_id
    ON email_template_versions (template_id);

-- ---------------------------------------------------------------------------
-- One newsletter list. `default_connection_id` is the default email
-- provider connection for the list's campaigns; a campaign may
-- override it with its own connection id.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS newsletter_lists (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    default_connection_id UUID REFERENCES email_connections (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    double_opt_in BOOLEAN NOT NULL DEFAULT FALSE,
    sender_name VARCHAR,
    sender_email VARCHAR,
    base_url VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- One subscriber of a list.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS newsletter_subscribers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id UUID NOT NULL REFERENCES newsletter_lists (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    email VARCHAR NOT NULL,
    name VARCHAR,
    status VARCHAR NOT NULL CHECK (status IN ('pending', 'confirmed', 'unsubscribed', 'bounced', 'complained')),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    confirmed_at TIMESTAMPTZ,
    unsubscribed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_newsletter_subscribers_list_email
    ON newsletter_subscribers (list_id, email);
CREATE INDEX IF NOT EXISTS idx_newsletter_subscribers_list_id
    ON newsletter_subscribers (list_id);
CREATE INDEX IF NOT EXISTS idx_newsletter_subscribers_status
    ON newsletter_subscribers (status);

-- ---------------------------------------------------------------------------
-- One campaign: a template sent to a list through a provider
-- connection, with denormalized send statistics.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS newsletter_campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id UUID NOT NULL REFERENCES newsletter_lists (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    name VARCHAR NOT NULL,
    template_id UUID REFERENCES email_templates (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    subject VARCHAR NOT NULL,
    preview_text VARCHAR,
    connection_id UUID REFERENCES email_connections (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    status VARCHAR NOT NULL CHECK (status IN ('draft', 'scheduled', 'sending', 'sent', 'completed', 'failed')),
    scheduled_at TIMESTAMPTZ,
    sent_at TIMESTAMPTZ,
    stats_sent BIGINT NOT NULL DEFAULT 0 CHECK (stats_sent >= 0),
    stats_delivered BIGINT NOT NULL DEFAULT 0 CHECK (stats_delivered >= 0),
    stats_opened BIGINT NOT NULL DEFAULT 0 CHECK (stats_opened >= 0),
    stats_clicked BIGINT NOT NULL DEFAULT 0 CHECK (stats_clicked >= 0),
    stats_bounced BIGINT NOT NULL DEFAULT 0 CHECK (stats_bounced >= 0),
    stats_unsubscribed BIGINT NOT NULL DEFAULT 0 CHECK (stats_unsubscribed >= 0),
    stats_complaints BIGINT NOT NULL DEFAULT 0 CHECK (stats_complaints >= 0),
    stats_unique_opened BIGINT NOT NULL DEFAULT 0 CHECK (stats_unique_opened >= 0),
    stats_unique_clicked BIGINT NOT NULL DEFAULT 0 CHECK (stats_unique_clicked >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_newsletter_campaigns_list_id
    ON newsletter_campaigns (list_id);
CREATE INDEX IF NOT EXISTS idx_newsletter_campaigns_status
    ON newsletter_campaigns (status);

-- ---------------------------------------------------------------------------
-- One tracking event: an open or a click on a campaign email.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS newsletter_tracking_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id UUID NOT NULL REFERENCES newsletter_campaigns (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    subscriber_id UUID NOT NULL REFERENCES newsletter_subscribers (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    kind VARCHAR NOT NULL CHECK (kind IN ('open', 'click')),
    url VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_newsletter_tracking_events_campaign_id
    ON newsletter_tracking_events (campaign_id);
CREATE INDEX IF NOT EXISTS idx_newsletter_tracking_events_subscriber_id
    ON newsletter_tracking_events (subscriber_id);
CREATE INDEX IF NOT EXISTS idx_newsletter_tracking_events_kind
    ON newsletter_tracking_events (kind);

-- Deferred foreign key: `mail_log.template_id` references
-- `email_templates`, which is created after `mail_log` in this file.
ALTER TABLE mail_log
    ADD CONSTRAINT fk_mail_log_template_id
    FOREIGN KEY (template_id) REFERENCES email_templates (id) ON DELETE SET NULL ON UPDATE NO ACTION;
