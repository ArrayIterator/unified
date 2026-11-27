-- Social marketing module (`src/modules/social_marketing/`):
-- Social marketing: admin-defined networks as data (API
-- base, auth type, publish path, supported post types,
-- text and media limits, rate limit), connected accounts
-- with write-only encrypted credentials, token expiry, and
-- a metrics-ingest webhook secret, scheduled posts with a
-- draft / scheduled / publishing / published / failed /
-- cancelled status machine publishing once per target
-- account through the generic HTTP adapter or the log-only
-- test adapter with one stored result per account,
-- campaigns grouping posts with a goal as data and live
-- post-count and metrics rollups, and analytics metrics
-- that arrive only through the provider verification-token
-- webhook or manual entry (never live-fetched), with an
-- append-only audit trail.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000121_sms_marketing:
-- the in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz.
--     `network_id` / `account_id` / `campaign_id` /
--     `post_id` reference social marketing rows by id with NO
--     foreign key, so this module must not assume any other
--     module's tables or migration order. `created_by` /
--     `actor` are plain TEXT with NO foreign key on purpose:
--     users live in the users module and this module must
--     not assume its table or migration order. `media_ids`
--     are document / storage ids by reference with NO
--     foreign key: the documents / storage modules own the
--     bytes.
--   - `tenant_id` is plain TEXT on every row: social data is
--     tenant business data and every query filters by it.
--     A row from another tenant reads as 404, never 403.
--   - Nothing is hard-coded: networks (including their API
--     base, auth type, publish path, supported post types,
--     text and media limits, per-network rate limit, and
--     enabled flag), accounts (including their handle and
--     token expiry), posts (including their type, text,
--     media references, link, and hashtags), campaigns
--     (including their goal label, goal metric, and goal
--     target), and metric names are all data rows. The only
--     fixed vocabulary is the provider kind (`http`,
--     `log_only`), the auth type (`none`, `bearer`,
--     `api_key_header`, `basic`), the post status (`draft`,
--     `scheduled`, `publishing`, `published`, `failed`,
--     `cancelled`), the campaign status (`draft`, `active`,
--     `paused`, `completed`, `cancelled`), and the metric
--     source (`webhook`, `manual`) - those are the shape of
--     the module, not its content.
--   - No money is stored anywhere in this module: no price,
--     cost, or charge column exists, and `f64` is never used
--     for a stored or returned number. Metric values and
--     goal targets are integer counts.
--   - Account credentials are write-only:
--     `credential_encrypted` holds only the `v1.`
--     HMAC-keystream ciphertext (the same format as the
--     holidays and SMS marketing modules),
--     `credential_fingerprint` holds only the first 12 hex
--     characters of the credential's SHA-256, and the raw
--     credential is never stored. `webhook_secret_hash`
--     holds only the SHA-256 hex of the metrics-ingest
--     webhook secret; the raw secret is returned exactly
--     once at creation / connect / rotation and never
--     stored. No real network name or credential is seeded
--     by this migration.
--   - Post text is stored as written; `media_ids` holds
--     document / storage ids only, never bytes. `results`
--     holds one publish result per target account
--     (`account_id`, provider post id, status, error,
--     published time); `error` holds the provider's failure
--     text truncated to 500 characters, never a credential.
--     `send_at` is the scheduled publish time and
--     `published_at` is when the publish pass ran. A
--     scheduled-post runner is NOT part of this module:
--     scheduling only records `send_at` and the
--     application emits `social.post.scheduled` for a
--     scheduler to pick up.
--   - Metrics are reported values only: rows arrive through
--     the provider verification-token webhook or manual
--     entry, never from a live fetch. One row per (post,
--     metric, source): re-ingesting the same metric from
--     the same source replaces the value instead of
--     double-counting it.
--   - Campaign rollups are computed live from the post and
--     metric rows, so no counter column is stored on the
--     campaign row.
--   - `user.deleted` anonymizes identity fields only:
--     account handles are the account's public handle on
--     its network and the deletion payload names no handle,
--     so no handle is redacted by deletion.
--   - Deleting a network, an account, or a campaign never
--     deletes the rows that reference it: posts keep their
--     network and campaign ids and their per-account
--     results, metric rows keep their post id, and audit
--     rows stay on record. Audit rows are likewise never
--     updated or deleted through the API.
--   - Unique indexes enforce the module invariants in the
--     database too: one network slug per tenant, one
--     account handle per (tenant, network), and one metric
--     row per (post, metric, source).
--   - This migration seeds no rows: a tenant with no
--     network rows cannot publish yet, a tenant with no
--     connected account cannot publish yet, and a tenant
--     with no metric rows shows zeroes, never invented
--     numbers.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Social networks: one admin-defined network per tenant.
-- `kind` is `http` (generic adapter: POST JSON to
-- `api_base_url` plus `publish_path` with the configured
-- auth) or `log_only` (test provider: no API base, no
-- network call). `auth_type` is `none`, `bearer`,
-- `api_key_header`, or `basic`; `auth_header` names the
-- header for api-key auth. `post_types` is the data list of
-- post types this network accepts; `max_text_length` and
-- `max_media_items` are the per-network content limits;
-- `posts_per_minute` is the per-network publish rate
-- limit. `request_fields` is an optional JSON object of
-- extra fields merged into every HTTP adapter request body.
CREATE TABLE IF NOT EXISTS social_networks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 100),
    kind TEXT NOT NULL CHECK (kind IN ('http', 'log_only')),
    api_base_url TEXT CHECK (api_base_url IS NULL OR char_length(api_base_url) BETWEEN 1 AND 2000),
    publish_path TEXT NOT NULL DEFAULT '/publish' CHECK (char_length(publish_path) BETWEEN 1 AND 2000),
    auth_type TEXT NOT NULL DEFAULT 'none' CHECK (auth_type IN ('none', 'bearer', 'api_key_header', 'basic')),
    auth_header TEXT CHECK (auth_header IS NULL OR char_length(auth_header) BETWEEN 1 AND 200),
    post_types JSONB NOT NULL DEFAULT '[]'::jsonb,
    max_text_length BIGINT NOT NULL DEFAULT 500 CHECK (max_text_length BETWEEN 1 AND 100000),
    max_media_items BIGINT NOT NULL DEFAULT 4 CHECK (max_media_items BETWEEN 0 AND 10),
    posts_per_minute BIGINT NOT NULL DEFAULT 30 CHECK (posts_per_minute BETWEEN 0 AND 100000),
    request_fields JSONB NOT NULL DEFAULT 'null'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind <> 'http' OR api_base_url IS NOT NULL),
    CHECK (kind <> 'log_only' OR api_base_url IS NULL)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_social_networks_tenant_slug ON social_networks (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_social_networks_tenant_kind ON social_networks (tenant_id, kind, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_social_networks_tenant_enabled ON social_networks (tenant_id, enabled, created_at DESC);

-- Social accounts: one connected account per tenant on a
-- network. `handle` is the account's public handle on its
-- network. `credential_encrypted` holds only the `v1.`
-- ciphertext; `credential_fingerprint` holds only the
-- first 12 hex characters of the credential's SHA-256.
-- `webhook_secret_hash` holds only the SHA-256 hex of the
-- metrics-ingest webhook secret. `token_expires_at` is
-- when the stored token stops being usable (NULL means it
-- never expires). `connected` is false after a disconnect,
-- which also clears the stored credential.
CREATE TABLE IF NOT EXISTS social_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    network_id UUID NOT NULL,
    handle TEXT NOT NULL CHECK (char_length(handle) BETWEEN 1 AND 100),
    display_name TEXT CHECK (display_name IS NULL OR char_length(display_name) BETWEEN 1 AND 200),
    auth_username TEXT CHECK (auth_username IS NULL OR char_length(auth_username) BETWEEN 1 AND 200),
    credential_encrypted TEXT,
    credential_fingerprint TEXT CHECK (credential_fingerprint IS NULL OR char_length(credential_fingerprint) = 12),
    webhook_secret_hash TEXT CHECK (webhook_secret_hash IS NULL OR char_length(webhook_secret_hash) = 64),
    token_expires_at TIMESTAMPTZ,
    connected BOOLEAN NOT NULL DEFAULT FALSE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    connected_at TIMESTAMPTZ,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_social_accounts_tenant_network_handle ON social_accounts (tenant_id, network_id, handle);
CREATE INDEX IF NOT EXISTS idx_social_accounts_tenant_network ON social_accounts (tenant_id, network_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_social_accounts_tenant_connected ON social_accounts (tenant_id, connected, created_at DESC);

-- Social posts: one post per tenant. `campaign_id` is an
-- optional campaign reference (no foreign key).
-- `network_id` is the network the post publishes through
-- (no foreign key). `account_ids` is a JSON array of
-- target account ids. `post_type` must be one of the
-- network row's `post_types`. `media_ids` is a JSON array
-- of document / storage ids (references only, no foreign
-- key). `hashtags` is a JSON array of normalized tags.
-- `status` is `draft`, `scheduled`, `publishing`,
-- `published`, `failed`, or `cancelled`; `send_at` is the
-- scheduled publish time and `published_at` is when the
-- publish pass ran. `results` is a JSON array of one
-- publish result per target account. `error` is the
-- provider's failure text truncated to 500 characters,
-- never a credential.
CREATE TABLE IF NOT EXISTS social_posts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    campaign_id UUID,
    network_id UUID NOT NULL,
    account_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    post_type TEXT NOT NULL CHECK (char_length(post_type) BETWEEN 1 AND 200),
    text TEXT NOT NULL CHECK (char_length(text) BETWEEN 1 AND 100000),
    media_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    link TEXT CHECK (link IS NULL OR char_length(link) BETWEEN 1 AND 2000),
    hashtags JSONB NOT NULL DEFAULT '[]'::jsonb,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'scheduled', 'publishing', 'published', 'failed', 'cancelled')),
    send_at TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    results JSONB NOT NULL DEFAULT '[]'::jsonb,
    error TEXT CHECK (error IS NULL OR char_length(error) BETWEEN 1 AND 500),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_social_posts_tenant_status ON social_posts (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_social_posts_tenant_network ON social_posts (tenant_id, network_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_social_posts_tenant_campaign ON social_posts (tenant_id, campaign_id, created_at ASC);

-- Social campaigns: one campaign per tenant grouping posts.
-- `goal` is a free-text goal label; `goal_metric` names
-- the metric the goal is measured in (any metric slug,
-- data) and `goal_target` is the integer count the
-- campaign aims for. `status` is `draft`, `active`,
-- `paused`, `completed`, or `cancelled`.
CREATE TABLE IF NOT EXISTS social_campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    goal TEXT NOT NULL CHECK (char_length(goal) BETWEEN 1 AND 5000),
    goal_metric TEXT CHECK (goal_metric IS NULL OR char_length(goal_metric) BETWEEN 1 AND 64),
    goal_target BIGINT CHECK (goal_target IS NULL OR goal_target >= 0),
    starts_at TIMESTAMPTZ,
    ends_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'paused', 'completed', 'cancelled')),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (starts_at IS NULL OR ends_at IS NULL OR starts_at <= ends_at)
);
CREATE INDEX IF NOT EXISTS idx_social_campaigns_tenant_status ON social_campaigns (tenant_id, status, created_at DESC);

-- Social metrics: one reported metric value per (post,
-- metric, source). Metrics arrive only through the
-- provider verification-token webhook (`webhook`) or
-- manual entry (`manual`); this module never fetches live
-- numbers. `account_id` names the account that reported
-- the value, when a webhook reported it.
CREATE TABLE IF NOT EXISTS social_metrics (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    post_id UUID NOT NULL,
    account_id UUID,
    metric TEXT NOT NULL CHECK (char_length(metric) BETWEEN 1 AND 64),
    value BIGINT NOT NULL DEFAULT 0 CHECK (value >= 0),
    source TEXT NOT NULL CHECK (source IN ('webhook', 'manual')),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_social_metrics_post_metric_source ON social_metrics (post_id, metric, source);
CREATE INDEX IF NOT EXISTS idx_social_metrics_tenant_post ON social_metrics (tenant_id, post_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_social_metrics_tenant_metric ON social_metrics (tenant_id, metric, created_at DESC);

-- Audit trail: one row per change to a network, an account,
-- a post, a campaign, or a metric (creates, updates,
-- deletes, connects, disconnects, schedules, publishes,
-- retries, cancels, webhook-secret rotations).
-- `entity_type` names the row kind; `entity_id` is the row
-- id as TEXT; `action` names the change; `before` / `after`
-- are the serialized rows (or NULL for creates / deletes).
-- Audit rows are never updated or deleted through the API.
CREATE TABLE IF NOT EXISTS social_audit (
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
CREATE INDEX IF NOT EXISTS idx_social_audit_tenant_created ON social_audit (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_social_audit_tenant_entity ON social_audit (tenant_id, entity_type, entity_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_social_audit_tenant_action ON social_audit (tenant_id, action, created_at DESC);
