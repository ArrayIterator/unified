-- Incident & On-Call tables for the Unified Pentagonal Project
-- (`src/modules/incidents.rs`): incidents, append-only timeline,
-- on-call rotations, on-call schedules, escalation policies,
-- per-incident escalation state, page attempts, postmortems, and
-- per-service auto-open rules.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/incidents.rs`, which
-- stay authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `incidents.created_by`, `incidents.acknowledged_by`,
--     `incidents.responders`, `incidents.current_responder`,
--     `incident_timeline.actor_id`,
--     `on_call_rotations.members`,
--     `on_call_schedules.member_id`, `page_attempts.paged_user_id`,
--     `incident_postmortems.created_by`, and
--     `postmortem_action_items.assignee_id` are plain VARCHAR user
--     ids: the module identifies callers by the `X-User-Id` header
--     and keeps user ids as strings, exactly like
--     `store_orders.user_id` in migration 000002. No foreign key
--     to `users` is stored here.
--   - `incidents.affected_service_ids` links to Service Monitor
--     service ids (plain VARCHAR - the Service Monitor owns
--     services); `incidents.source_service_id` names the service
--     for auto-opened incidents. `auto_open_rules.service_id` is
--     the same service id and is the primary key of its table.
--   - `incidents.severity` is `critical`, `high`, `medium`, or
--     `low` (`IncidentSeverity`); `incidents.status` is `open`,
--     `acknowledged`, `investigating`, `mitigating`, `resolved`,
--     or `closed` (`IncidentStatus`) - the module validates the
--     transition (`open` -> `acknowledged` -> `investigating` ->
--     `mitigating` -> `resolved` -> `closed`, with `resolved` ->
--     `investigating` reopen and `investigating` ->
--     `acknowledged` step-back); the CHECK below only bounds the
--     value set. `incidents.source` is `manual` or `auto`.
--   - `incident_timeline` is append-only by design: rows are never
--     updated or deleted by the module. `kind` is `note`,
--     `status_change`, `responder_action`, `page`, `escalation`,
--     or `system`.
--   - `on_call_schedules.kind` is `regular` or `override`; an
--     `override` entry wins over a `regular` entry covering the
--     same time (resolved in the module). `handover_time` is
--     "HH:MM" in 24-hour form (validated in the module, enforced
--     by the CHECK below).
--   - `escalation_policies.steps` is JSONB holding the ordered
--     `EscalationStep` list (`after_minutes` strictly increasing
--     in the module; `target` is `responder`, `manager`, or
--     `user`). `incident_escalations.status` is `active`,
--     `completed`, or `stopped`.
--   - `page_attempts.status` is `sent`, `acknowledged`, or
--     `expired`.
--   - `incident_postmortems` has one row per incident (the module
--     refuses a second postmortem; the UNIQUE below enforces it).
--     Action items live in `postmortem_action_items`; `status` is
--     `open`, `in_progress`, or `done`. `task_reference` is empty
--     until the tasks module consumes the
--     `tasks.create.requested` event the module emits (no tasks
--     listener consumes it yet - see the module header).
--   - `auto_open_rules.enabled` defaults to `false`: auto-open is
--     opt-in per service. `auto_resolve_on_recovery` is also off
--     by default.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One incident.
CREATE TABLE IF NOT EXISTS incidents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    description TEXT,
    severity VARCHAR(20) NOT NULL DEFAULT 'medium' CHECK (severity IN ('critical', 'high', 'medium', 'low')),
    status VARCHAR(20) NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'acknowledged', 'investigating', 'mitigating', 'resolved', 'closed')),
    affected_service_ids TEXT[] NOT NULL DEFAULT '{}',
    responders TEXT[] NOT NULL DEFAULT '{}',
    current_responder VARCHAR(255),
    source VARCHAR(20) NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'auto')),
    source_service_id VARCHAR(255),
    escalation_policy_id UUID,
    rotation_id UUID,
    created_by VARCHAR(255) NOT NULL,
    acknowledged_by VARCHAR(255),
    acknowledged_at TIMESTAMPTZ,
    resolved_at TIMESTAMPTZ,
    closed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_incidents_status ON incidents (status);
CREATE INDEX IF NOT EXISTS idx_incidents_severity ON incidents (severity);
CREATE INDEX IF NOT EXISTS idx_incidents_source ON incidents (source);
CREATE INDEX IF NOT EXISTS idx_incidents_source_service_id ON incidents (source_service_id);
CREATE INDEX IF NOT EXISTS idx_incidents_current_responder ON incidents (current_responder);
CREATE INDEX IF NOT EXISTS idx_incidents_responders ON incidents USING GIN (responders);
CREATE INDEX IF NOT EXISTS idx_incidents_affected_services ON incidents USING GIN (affected_service_ids);

-- One append-only timeline entry.
CREATE TABLE IF NOT EXISTS incident_timeline (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    incident_id UUID NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
    actor_id VARCHAR(255) NOT NULL,
    kind VARCHAR(32) NOT NULL CHECK (kind IN ('note', 'status_change', 'responder_action', 'page', 'escalation', 'system')),
    message TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_incident_timeline_incident_id ON incident_timeline (incident_id);
CREATE INDEX IF NOT EXISTS idx_incident_timeline_kind ON incident_timeline (kind);
CREATE INDEX IF NOT EXISTS idx_incident_timeline_created_at ON incident_timeline (created_at);

-- One on-call rotation.
CREATE TABLE IF NOT EXISTS on_call_rotations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    members TEXT[] NOT NULL DEFAULT '{}',
    handover_interval_days BIGINT NOT NULL DEFAULT 7 CHECK (handover_interval_days >= 1 AND handover_interval_days <= 365),
    handover_time VARCHAR(5) NOT NULL DEFAULT '09:00' CHECK (handover_time ~ '^(0[0-9]|1[0-9]|2[0-3]):[0-5][0-9]$'),
    timezone VARCHAR(64) NOT NULL DEFAULT 'UTC',
    current_index BIGINT NOT NULL DEFAULT 0 CHECK (current_index >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_on_call_rotations_name ON on_call_rotations (name);

-- One on-call schedule entry.
CREATE TABLE IF NOT EXISTS on_call_schedules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rotation_id UUID NOT NULL REFERENCES on_call_rotations (id) ON DELETE CASCADE,
    member_id VARCHAR(255) NOT NULL,
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    kind VARCHAR(20) NOT NULL DEFAULT 'regular' CHECK (kind IN ('regular', 'override')),
    overrides_entry_id UUID REFERENCES on_call_schedules (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS idx_on_call_schedules_rotation_id ON on_call_schedules (rotation_id);
CREATE INDEX IF NOT EXISTS idx_on_call_schedules_member_id ON on_call_schedules (member_id);
CREATE INDEX IF NOT EXISTS idx_on_call_schedules_window ON on_call_schedules (starts_at, ends_at);

-- One escalation policy.
CREATE TABLE IF NOT EXISTS escalation_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    applies_to_severities TEXT[] NOT NULL DEFAULT '{}',
    steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_escalation_policies_name ON escalation_policies (name);

-- Per-incident escalation state.
CREATE TABLE IF NOT EXISTS incident_escalations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    incident_id UUID NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
    policy_id UUID NOT NULL REFERENCES escalation_policies (id) ON DELETE RESTRICT,
    step_index BIGINT NOT NULL DEFAULT 0 CHECK (step_index >= 0),
    next_escalation_at TIMESTAMPTZ NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'completed', 'stopped')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_incident_escalations_incident_id ON incident_escalations (incident_id);
CREATE INDEX IF NOT EXISTS idx_incident_escalations_due ON incident_escalations (status, next_escalation_at);

-- One page attempt.
CREATE TABLE IF NOT EXISTS page_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    incident_id UUID NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
    paged_user_id VARCHAR(255) NOT NULL,
    channel VARCHAR(64) NOT NULL DEFAULT 'notification',
    status VARCHAR(20) NOT NULL DEFAULT 'sent' CHECK (status IN ('sent', 'acknowledged', 'expired')),
    acknowledged_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_page_attempts_incident_id ON page_attempts (incident_id);
CREATE INDEX IF NOT EXISTS idx_page_attempts_paged_user_id ON page_attempts (paged_user_id);
CREATE INDEX IF NOT EXISTS idx_page_attempts_status ON page_attempts (status);

-- One postmortem (one per incident).
CREATE TABLE IF NOT EXISTS incident_postmortems (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    incident_id UUID NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
    summary TEXT NOT NULL,
    timeline_summary TEXT,
    root_cause TEXT,
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (incident_id)
);

CREATE INDEX IF NOT EXISTS idx_incident_postmortems_incident_id ON incident_postmortems (incident_id);

-- One postmortem action item.
CREATE TABLE IF NOT EXISTS postmortem_action_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    postmortem_id UUID NOT NULL REFERENCES incident_postmortems (id) ON DELETE CASCADE,
    title VARCHAR(200) NOT NULL,
    assignee_id VARCHAR(255),
    status VARCHAR(20) NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'in_progress', 'done')),
    task_reference VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_postmortem_action_items_postmortem_id ON postmortem_action_items (postmortem_id);
CREATE INDEX IF NOT EXISTS idx_postmortem_action_items_status ON postmortem_action_items (status);

-- Per-service auto-open rule (opt-in; off by default).
CREATE TABLE IF NOT EXISTS auto_open_rules (
    service_id VARCHAR(255) PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    severity VARCHAR(20) NOT NULL DEFAULT 'medium' CHECK (severity IN ('critical', 'high', 'medium', 'low')),
    auto_resolve_on_recovery BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Link incidents to their escalation policy and rotation now that
-- the referenced tables exist.
ALTER TABLE incidents
    ADD CONSTRAINT fk_incidents_escalation_policy
    FOREIGN KEY (escalation_policy_id) REFERENCES escalation_policies (id) ON DELETE SET NULL
    NOT VALID;

ALTER TABLE incidents
    ADD CONSTRAINT fk_incidents_rotation
    FOREIGN KEY (rotation_id) REFERENCES on_call_rotations (id) ON DELETE SET NULL
    NOT VALID;
