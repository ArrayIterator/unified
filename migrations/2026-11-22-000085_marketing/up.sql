-- Marketing Automation tables for the Unified Pentagonal Project
-- (`src/modules/marketing.rs`): campaigns and their A/B variants,
-- dynamic segments (rule trees stored as JSONB), drip sequences
-- with branching steps, per-contact enrollments, lead scores with
-- config-driven scoring rules, and the send audit trail.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/marketing.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `marketing_campaigns.status` is `draft`, `scheduled`,
--     `sending`, `sent`, or `paused`. `segment_id` references a
--     segment by UUID with NO foreign key on purpose: segments are
--     an in-memory store until this migration is executed, and the
--     module validates the reference at the API layer. The same
--     applies to every other cross-table UUID reference below.
--   - `newsletter_list_id` is a reference to a newsletter list
--     (owned by the newsletter module) for reporting only;
--     marketing sends go through the mail module, never directly
--     to a newsletter list.
--   - `marketing_campaign_variants.weight_percent` is the share of
--     recipients (0-100); variant weights must sum to at most 100
--     and the remainder goes to the campaign base content. The
--     module enforces this at the API layer.
--   - `marketing_segments.rule_tree` is JSONB: a group
--     `{"operator": "and"|"or", "rules": [...]}` or a leaf
--     `{"field": ..., "op": ..., "value": ...}`. The module
--     validates the tree against its configured field/operator
--     allowlists before storing it.
--   - `marketing_segment_size_history` is append-only: one row per
--     evaluation run, recording the segment size and how many
--     contacts were actually evaluated (the known audience), so a
--     size is never mistaken for full CRM coverage.
--   - `marketing_sequences.trigger_kind` is `manual`,
--     `form_submitted`, `purchase`, or `tag_added`, with
--     `trigger_form_id` / `trigger_tag` holding the trigger detail.
--   - `marketing_sequence_steps.condition` is JSONB:
--     `{"type": "always"|"opened"|"clicked"|"did_not_open"|
--     "did_not_click", "step_id": "...", "window_hours": 72}`.
--   - `marketing_enrollments.history` is a JSONB array of per-step
--     events (`sent`, `skipped`, `waiting`, `completed`).
--   - `marketing_scores.events` is a JSONB array of scored actions
--     (`action`, `points`, `rule_id`, `at`). The effective score is
--     recomputed from these events with each rule's linear decay
--     applied; `score` stores the last computed value.
--   - `marketing_score_rules.points` may be negative (penalties).
--     `decay_days` NULL means no decay.
--   - `marketing_sends` is the append-only send audit trail:
--     every accepted send and every consent/suppression skip is
--     recorded here. `status` is `queued`, `sent`, `failed`,
--     `skipped`, `opened`, `clicked`, `bounced`, or `unsubscribed`.
--     `skip_reason` names why a skipped send never went out
--     (`unsubscribed`, `consent_withdrawn`, `consent_not_granted`,
--     `consent_unverified`).
--   - No money columns here. All ids are UUID, all timestamps are
--     Timestamptz. `user_id` / actor columns are TEXT with NO
--     foreign key on purpose: actor ids in this codebase are opaque
--     strings (the `X-User-Id` header), not necessarily the UUID
--     primary key of the `users` table.

CREATE TABLE IF NOT EXISTS marketing_campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    subject VARCHAR(500) NOT NULL,
    body_text TEXT,
    body_html TEXT,
    template_id TEXT,
    segment_id UUID,
    newsletter_list_id TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'draft',
    scheduled_at TIMESTAMPTZ,
    sent_at TIMESTAMPTZ,
    ab_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    ab_winner_metric VARCHAR(20) NOT NULL DEFAULT 'open_rate',
    ab_min_sample BIGINT,
    winning_variant_id UUID,
    stats_queued BIGINT NOT NULL DEFAULT 0,
    stats_sent BIGINT NOT NULL DEFAULT 0,
    stats_delivered BIGINT NOT NULL DEFAULT 0,
    stats_failed BIGINT NOT NULL DEFAULT 0,
    stats_skipped BIGINT NOT NULL DEFAULT 0,
    stats_opened BIGINT NOT NULL DEFAULT 0,
    stats_clicked BIGINT NOT NULL DEFAULT 0,
    stats_unsubscribed BIGINT NOT NULL DEFAULT 0,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_campaigns_status ON marketing_campaigns (status);
CREATE INDEX IF NOT EXISTS idx_marketing_campaigns_segment ON marketing_campaigns (segment_id);
CREATE INDEX IF NOT EXISTS idx_marketing_campaigns_scheduled ON marketing_campaigns (scheduled_at) WHERE status = 'scheduled';

CREATE TABLE IF NOT EXISTS marketing_campaign_variants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id UUID NOT NULL,
    name VARCHAR(200) NOT NULL,
    subject VARCHAR(500) NOT NULL,
    body_text TEXT,
    body_html TEXT,
    weight_percent SMALLINT NOT NULL DEFAULT 50,
    sent BIGINT NOT NULL DEFAULT 0,
    opened BIGINT NOT NULL DEFAULT 0,
    clicked BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_variants_campaign ON marketing_campaign_variants (campaign_id);

CREATE TABLE IF NOT EXISTS marketing_segments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    rule_tree JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(20) NOT NULL DEFAULT 'active',
    last_evaluated_at TIMESTAMPTZ,
    last_size BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_segments_status ON marketing_segments (status);

CREATE TABLE IF NOT EXISTS marketing_segment_size_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    segment_id UUID NOT NULL,
    size BIGINT NOT NULL DEFAULT 0,
    evaluated_contacts BIGINT NOT NULL DEFAULT 0,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_segment_history_segment ON marketing_segment_size_history (segment_id, recorded_at DESC);

CREATE TABLE IF NOT EXISTS marketing_sequences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    trigger_kind VARCHAR(20) NOT NULL DEFAULT 'manual',
    trigger_form_id TEXT,
    trigger_tag TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_sequences_status ON marketing_sequences (status);
CREATE INDEX IF NOT EXISTS idx_marketing_sequences_trigger ON marketing_sequences (trigger_kind);

CREATE TABLE IF NOT EXISTS marketing_sequence_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sequence_id UUID NOT NULL,
    step_order BIGINT NOT NULL DEFAULT 1,
    delay_seconds BIGINT NOT NULL DEFAULT 0,
    condition JSONB NOT NULL DEFAULT '{"type": "always"}'::jsonb,
    subject VARCHAR(500) NOT NULL,
    body_text TEXT,
    body_html TEXT,
    template_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_steps_sequence ON marketing_sequence_steps (sequence_id, step_order);

CREATE TABLE IF NOT EXISTS marketing_enrollments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sequence_id UUID NOT NULL,
    contact_id TEXT NOT NULL,
    email VARCHAR(254) NOT NULL,
    next_step BIGINT NOT NULL DEFAULT 1,
    status VARCHAR(20) NOT NULL DEFAULT 'active',
    next_step_at TIMESTAMPTZ,
    history JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_enrollments_sequence ON marketing_enrollments (sequence_id, status);
CREATE INDEX IF NOT EXISTS idx_marketing_enrollments_due ON marketing_enrollments (next_step_at) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_marketing_enrollments_contact ON marketing_enrollments (contact_id);

CREATE TABLE IF NOT EXISTS marketing_scores (
    contact_id TEXT PRIMARY KEY,
    email VARCHAR(254) NOT NULL,
    score BIGINT NOT NULL DEFAULT 0,
    warm BOOLEAN NOT NULL DEFAULT FALSE,
    hot BOOLEAN NOT NULL DEFAULT FALSE,
    hot_notified_at TIMESTAMPTZ,
    last_action_at TIMESTAMPTZ,
    events JSONB NOT NULL DEFAULT '[]'::jsonb,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_scores_hot ON marketing_scores (hot);
CREATE INDEX IF NOT EXISTS idx_marketing_scores_score ON marketing_scores (score DESC);

CREATE TABLE IF NOT EXISTS marketing_score_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    action VARCHAR(200) NOT NULL,
    points BIGINT NOT NULL DEFAULT 0,
    decay_days BIGINT,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_score_rules_action ON marketing_score_rules (action, active);

CREATE TABLE IF NOT EXISTS marketing_sends (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id UUID,
    sequence_id UUID,
    step_id UUID,
    enrollment_id UUID,
    contact_id TEXT NOT NULL,
    email VARCHAR(254) NOT NULL,
    variant_id UUID,
    status VARCHAR(20) NOT NULL DEFAULT 'queued',
    mail_job_id TEXT,
    skip_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_marketing_sends_campaign ON marketing_sends (campaign_id, status);
CREATE INDEX IF NOT EXISTS idx_marketing_sends_sequence ON marketing_sends (sequence_id, step_id);
CREATE INDEX IF NOT EXISTS idx_marketing_sends_contact ON marketing_sends (contact_id);
CREATE INDEX IF NOT EXISTS idx_marketing_sends_mail_job ON marketing_sends (mail_job_id);
