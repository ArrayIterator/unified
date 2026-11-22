-- Surveys / NPS / CSAT tables for the Unified Pentagonal Project
-- (`src/modules/surveys.rs`): survey versions, questions,
-- distributions, tokenized responses, and answers.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/surveys.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `surveys` holds one row per version. A family shares
--     `family_id` across versions; version 1 has
--     `family_id = id`. `supersedes` points at the previous
--     version row. `status` is `draft`, `published`, or
--     `archived`; publishing a version archives the previously
--     published version of the same family. The unique index on
--     (family_id, version) keeps version numbers gap-free per
--     family. `created_by` is a plain VARCHAR user id: the module
--     identifies callers by the `X-User-Id` header, exactly like
--     `store_orders.user_id` in migration 000002. No foreign key
--     to `users` is stored here.
--   - `survey_questions.question_type` is `nps`, `csat`, `rating`,
--     `text`, or `multiple_choice`. `options` (JSONB) applies to
--     `multiple_choice` only. `min_value` / `max_value` record the
--     score bounds the question was created with (NPS 0–10, CSAT
--     and rating 1–5). `position` orders the questions inside one
--     survey version; the unique index on (survey_id, position)
--     keeps positions distinct per version.
--   - `survey_distributions.trigger` is `manual`, `ticket_closed`,
--     `purchase`, or `scheduled`. `department_id`, `product_id`,
--     and `agent_id` are plain VARCHAR references used as trigger
--     filters (no foreign keys: the departments / products /
--     users modules own their rows). `responsible_user_id`
--     receives low-score follow-up notices. `scheduled_at` is
--     required for `scheduled` distributions. `expires_in_days`
--     is the token lifetime for invitations from this
--     distribution (1..365, default 30). `status` is `pending`,
--     `sent`, `completed`, or `cancelled`; `scheduler_status` is
--     `not_scheduled`, `queued`, or `declined` (only meaningful
--     for `scheduled` distributions).
--   - `survey_responses` holds one row per recipient per survey
--     version: the unique index on (survey_id, recipient_id)
--     enforces that. `token` is the unguessable response link
--     token (32 random bytes, base64url) and is unique;
--     `token_expires_at` bounds its lifetime. `submitted_at` is
--     NULL until the recipient answers. `follow_up_status` is
--     `none`, `requested`, or `created`; `follow_up_task_id` is
--     the tasks module's task id when it answered
--     `task.create_requested` (plain VARCHAR reference, no
--     foreign key: the tasks module owns tasks).
--     `recipient_id`, `agent_id`, `department_id`, and
--     `product_id` are plain VARCHAR references (no foreign keys).
--   - `survey_answers.score` holds NPS (0–10), CSAT (1–5), and
--     rating (1–5) answers; `text` holds free-text answers;
--     `choice` holds multiple-choice answers. The CHECK enforces
--     the score ranges and that a `choice` is only stored when a
--     score is not. NPS and CSAT are computed on read, never
--     stored: NPS is %promoters minus %detractors, CSAT is
--     % satisfied (score 4–5).

CREATE TABLE IF NOT EXISTS surveys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    family_id UUID NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    supersedes UUID REFERENCES surveys (id) ON DELETE SET NULL,
    title VARCHAR(200) NOT NULL,
    description TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published', 'archived')),
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS surveys_family_version_idx ON surveys (family_id, version);
CREATE INDEX IF NOT EXISTS surveys_status_idx ON surveys (status);
CREATE INDEX IF NOT EXISTS surveys_created_by_idx ON surveys (created_by);

CREATE TABLE IF NOT EXISTS survey_questions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    survey_id UUID NOT NULL REFERENCES surveys (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 1),
    question_type VARCHAR(20) NOT NULL CHECK (question_type IN ('nps', 'csat', 'rating', 'text', 'multiple_choice')),
    text VARCHAR(1000) NOT NULL,
    required BOOLEAN NOT NULL DEFAULT FALSE,
    options JSONB NOT NULL DEFAULT '[]'::jsonb,
    min_value INTEGER CHECK (min_value IS NULL OR min_value BETWEEN 0 AND 10),
    max_value INTEGER CHECK (max_value IS NULL OR max_value BETWEEN 0 AND 10),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (max_value IS NULL OR min_value IS NULL OR min_value <= max_value)
);

CREATE UNIQUE INDEX IF NOT EXISTS survey_questions_position_idx ON survey_questions (survey_id, position);
CREATE INDEX IF NOT EXISTS survey_questions_survey_idx ON survey_questions (survey_id);

CREATE TABLE IF NOT EXISTS survey_distributions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    survey_id UUID NOT NULL REFERENCES surveys (id) ON DELETE CASCADE,
    trigger VARCHAR(20) NOT NULL CHECK (trigger IN ('manual', 'ticket_closed', 'purchase', 'scheduled')),
    department_id VARCHAR(255),
    product_id VARCHAR(255),
    agent_id VARCHAR(255),
    responsible_user_id VARCHAR(255),
    scheduled_at TIMESTAMPTZ,
    expires_in_days INTEGER NOT NULL DEFAULT 30 CHECK (expires_in_days BETWEEN 1 AND 365),
    status VARCHAR(20) NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'completed', 'cancelled')),
    scheduler_status VARCHAR(20) NOT NULL DEFAULT 'not_scheduled' CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    scheduler_job_id VARCHAR(255),
    sent_count BIGINT NOT NULL DEFAULT 0 CHECK (sent_count >= 0),
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (trigger <> 'scheduled' OR scheduled_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS survey_distributions_survey_idx ON survey_distributions (survey_id);
CREATE INDEX IF NOT EXISTS survey_distributions_trigger_idx ON survey_distributions (trigger, status);

CREATE TABLE IF NOT EXISTS survey_responses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    survey_id UUID NOT NULL REFERENCES surveys (id) ON DELETE CASCADE,
    distribution_id UUID REFERENCES survey_distributions (id) ON DELETE SET NULL,
    recipient_id VARCHAR(255) NOT NULL,
    recipient_email VARCHAR(254),
    agent_id VARCHAR(255),
    department_id VARCHAR(255),
    product_id VARCHAR(255),
    token VARCHAR(255) NOT NULL,
    token_expires_at TIMESTAMPTZ NOT NULL,
    submitted_at TIMESTAMPTZ,
    follow_up_status VARCHAR(20) NOT NULL DEFAULT 'none' CHECK (follow_up_status IN ('none', 'requested', 'created')),
    follow_up_task_id VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS survey_responses_token_idx ON survey_responses (token);
CREATE UNIQUE INDEX IF NOT EXISTS survey_responses_recipient_version_idx ON survey_responses (survey_id, recipient_id);
CREATE INDEX IF NOT EXISTS survey_responses_survey_idx ON survey_responses (survey_id);
CREATE INDEX IF NOT EXISTS survey_responses_distribution_idx ON survey_responses (distribution_id);
CREATE INDEX IF NOT EXISTS survey_responses_submitted_idx ON survey_responses (submitted_at) WHERE submitted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS survey_responses_agent_idx ON survey_responses (agent_id) WHERE agent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS survey_responses_department_idx ON survey_responses (department_id) WHERE department_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS survey_responses_product_idx ON survey_responses (product_id) WHERE product_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS survey_answers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    response_id UUID NOT NULL REFERENCES survey_responses (id) ON DELETE CASCADE,
    question_id UUID NOT NULL REFERENCES survey_questions (id) ON DELETE CASCADE,
    score INTEGER CHECK (score IS NULL OR score BETWEEN 0 AND 10),
    text TEXT,
    choice VARCHAR(200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (score IS NULL OR choice IS NULL)
);

CREATE INDEX IF NOT EXISTS survey_answers_response_idx ON survey_answers (response_id);
CREATE INDEX IF NOT EXISTS survey_answers_question_idx ON survey_answers (question_id);
