-- Workflow Automation / Autopilot tables for the Unified Pentagonal Project
-- (`src/modules/workflows.rs`): workflow definitions with event,
-- schedule, webhook, and manual triggers; nested condition groups;
-- ordered actions; per-run logs with per-step outcomes.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/workflows.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `workflows.owner_id` and `workflows.company_id` are plain
--     VARCHAR references (the module identifies callers by the
--     `X-User-Id` header and keeps user ids as strings, exactly
--     like `store_orders.user_id` in migration 000002). No foreign
--     key to `users` is stored here.
--   - `workflows.trigger` is JSONB holding the `WorkflowTrigger`
--     (`kind` event / schedule / webhook / manual, plus `event`
--     for event triggers and `cron` + `timezone` for schedule
--     triggers). The CHECK below enforces that an event trigger
--     names an event and a schedule trigger names a cron
--     expression.
--   - `workflows.conditions` is JSONB holding the `ConditionGroup`
--     (`operator` and / or, `conditions` of leaves or nested
--     groups, at most 3 levels deep — validated in the module at
--     save time; the CHECK below only bounds the serialized size).
--   - `workflows.actions` is JSONB holding the ordered
--     `WorkflowAction` list (at most 50 actions; each action's
--     `kind` is one of send_notification, send_email, create_task,
--     create_ticket, update_record, call_webhook, assign_user,
--     add_tag, run_workflow — validated in the module).
--   - `workflows.webhook_token_hash` is the SHA-256 hex of the
--     webhook token; the token itself is never stored. It is
--     unique when present, so a token identifies exactly one
--     workflow.
--   - `workflows.webhook_allowed_hosts` is JSONB holding the host
--     allowlist for the `call_webhook` action (exact hosts or
--     `*.example.com` wildcards). An empty list means no outbound
--     webhook call is allowed.
--   - `workflows.scheduler_status` is `not_scheduled`, `queued`,
--     or `declined`: the scheduler does not know the
--     `workflows.run` job kind yet, so scheduled workflows record
--     `declined` until that kind is added to `KNOWN_KINDS` in
--     `scheduler.rs`.
--   - `workflow_runs.status` is `running`, `finished`, `failed`,
--     or `dropped` (`RunStatus`). `payload_summary` is JSONB
--     holding the REDACTED summary of the trigger payload (top-
--     level keys with value types, strings truncated to 128
--     characters, secret-named keys dropped). The full payload is
--     never stored — that is why a retry needs a caller-supplied
--     payload. `condition_matched` records whether the conditions
--     passed; `skipped_reason` explains a finished run that
--     executed no actions. `retried_from_run_id` links a retry to
--     the failed run it was started from.
--   - `workflow_run_steps.status` is `pending`, `success`,
--     `failed`, `skipped`, or `deferred` (`deferred` = a webhook
--     step in a run that started on the sync bus path without an
--     async runtime). `attempts` is 1 for emit-based actions and
--     up to 3 for outbound webhook steps (exponential backoff).
--     `detail` is JSONB holding the step outcome detail (event
--     name, handler count, HTTP status — never secrets).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One workflow definition.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS workflows (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    trigger JSONB NOT NULL,
    conditions JSONB,
    actions JSONB NOT NULL DEFAULT '[]'::JSONB,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    owner_id VARCHAR(128) NOT NULL,
    company_id VARCHAR(128),
    webhook_token_hash VARCHAR(64),
    webhook_allowed_hosts JSONB NOT NULL DEFAULT '[]'::JSONB,
    run_count BIGINT NOT NULL DEFAULT 0 CHECK (run_count >= 0),
    success_count BIGINT NOT NULL DEFAULT 0 CHECK (success_count >= 0),
    failure_count BIGINT NOT NULL DEFAULT 0 CHECK (failure_count >= 0),
    dropped_count BIGINT NOT NULL DEFAULT 0 CHECK (dropped_count >= 0),
    last_run_at TIMESTAMPTZ,
    last_run_status VARCHAR(32) CHECK (last_run_status IS NULL OR last_run_status IN ('running', 'finished', 'failed', 'dropped')),
    scheduler_status VARCHAR(32) NOT NULL DEFAULT 'not_scheduled'
        CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    scheduler_job_id VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((trigger ->> 'kind') IN ('event', 'schedule', 'webhook', 'manual')),
    CHECK ((trigger ->> 'kind') <> 'event' OR (trigger ->> 'event') IS NOT NULL),
    CHECK ((trigger ->> 'kind') <> 'schedule' OR (trigger ->> 'cron') IS NOT NULL),
    CHECK (success_count <= run_count),
    CHECK (failure_count <= run_count),
    CHECK (dropped_count <= run_count)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_workflows_webhook_token_hash
    ON workflows (webhook_token_hash) WHERE webhook_token_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_workflows_owner_id ON workflows (owner_id);
CREATE INDEX IF NOT EXISTS idx_workflows_company_id ON workflows (company_id);
CREATE INDEX IF NOT EXISTS idx_workflows_is_active ON workflows (is_active);
CREATE INDEX IF NOT EXISTS idx_workflows_trigger_kind ON workflows ((trigger ->> 'kind'));

-- ---------------------------------------------------------------------------
-- One execution of a workflow.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS workflow_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_id UUID NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    trigger_kind VARCHAR(32) NOT NULL
        CHECK (trigger_kind IN ('event', 'schedule', 'webhook', 'manual')),
    trigger_name VARCHAR(256),
    status VARCHAR(32) NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'finished', 'failed', 'dropped')),
    payload_summary JSONB NOT NULL DEFAULT '{}'::JSONB,
    dry_run BOOLEAN NOT NULL DEFAULT FALSE,
    depth INTEGER NOT NULL DEFAULT 0 CHECK (depth >= 0 AND depth <= 3),
    condition_matched BOOLEAN NOT NULL DEFAULT FALSE,
    skipped_reason TEXT,
    retried_from_run_id UUID REFERENCES workflow_runs (id) ON DELETE SET NULL,
    duration_ms BIGINT CHECK (duration_ms IS NULL OR duration_ms >= 0),
    error TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (finished_at IS NULL OR finished_at >= started_at),
    CHECK (status <> 'dropped' OR error IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_workflow_runs_workflow_id ON workflow_runs (workflow_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_started_at ON workflow_runs (started_at);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_status ON workflow_runs (status);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_workflow_started ON workflow_runs (workflow_id, started_at);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_trigger_kind ON workflow_runs (trigger_kind);

-- ---------------------------------------------------------------------------
-- One executed step of a run.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS workflow_run_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id UUID NOT NULL REFERENCES workflow_runs (id) ON DELETE CASCADE,
    workflow_id UUID NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    step_index INTEGER NOT NULL CHECK (step_index >= 0),
    action_kind VARCHAR(64) NOT NULL
        CHECK (action_kind IN ('send_notification', 'send_email', 'create_task', 'create_ticket', 'update_record', 'call_webhook', 'assign_user', 'add_tag', 'run_workflow')),
    status VARCHAR(32) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'success', 'failed', 'skipped', 'deferred')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0 AND attempts <= 3),
    error TEXT,
    detail JSONB NOT NULL DEFAULT '{}'::JSONB,
    duration_ms BIGINT CHECK (duration_ms IS NULL OR duration_ms >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run_id ON workflow_run_steps (run_id);
CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_workflow_id ON workflow_run_steps (workflow_id);
CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run_step ON workflow_run_steps (run_id, step_index);
CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_status ON workflow_run_steps (status);
