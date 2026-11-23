-- Project Management module tables for the Unified Pentagonal Project
-- (`src/modules/projects.rs`): projects with selectable
-- methodologies, boards and columns, milestones, sprints, phases,
-- cycles, program increments, business cases, tasks with subtasks
-- and dependencies, comments, watchers, attachments, time entries,
-- automation rules, templates, and the activity trail.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `projects.methodology` is one of the 15 selectable
--     methodologies: `kanban`, `scrum`, `waterfall`, `scrumban`,
--     `agile`, `lean`, `xp`, `prince2`, `pmbok`, `cpm`, `ccpm`,
--     `six_sigma`, `hybrid`, `shape_up`, or `safe`. The methodology
--     selects which companion records a project uses (sprints for
--     Scrum, phases and gates for Waterfall and PRINCE2, cycles and
--     appetite for Shape Up, program increments for SAFe,
--     DMAIC phases for Six Sigma); nothing in the schema forbids a
--     project from holding records of another methodology.
--   - `project_tasks` is self-referential twice: `parent_id` is the
--     subtask parent (ON DELETE CASCADE) and `epic_id` is the
--     containing epic (ON DELETE SET NULL). `sprint_id`,
--     `phase_id`, `cycle_id`, `pi_id`, and `milestone_id` are all
--     ON DELETE SET NULL so deleting a container unlinks its tasks
--     instead of deleting them.
--   - `project_task_dependencies` forbids a task depending on
--     itself (`task_id <> depends_on_task_id`). Cycle detection
--     across several tasks is the module's job, not the schema's.
--   - `project_time_entries.hours` is DOUBLE PRECISION on purpose
--     and is NOT money: it is a duration in hours, capped at 24
--     per entry by CHECK. Billable time is billed through the
--     billing module; `invoice_id` is a plain TEXT reference with
--     NO foreign key on purpose, so deleting an invoice never
--     deletes the time record that was billed on it.
--   - `project_task_attachments` stores only references: `target`
--     and `key` identify the storage object, `stored_key` is the
--     key under which the file lives in the storage module. No
--     file bytes live in this schema.
--   - `project_templates` holds the seed boards, columns, statuses,
--     phases, and custom fields a methodology ships with, so a new
--     project can start from a template instead of an empty board.
--     `is_system` marks the templates shipped with the module.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One project. The methodology selects the working style; the status
-- and visibility are plain state, not a workflow.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS projects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    methodology TEXT NOT NULL
        CHECK (methodology IN ('kanban', 'scrum', 'waterfall', 'scrumban', 'agile', 'lean', 'xp', 'prince2', 'pmbok', 'cpm', 'ccpm', 'six_sigma', 'hybrid', 'shape_up', 'safe')),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'on_hold', 'completed', 'cancelled')),
    visibility TEXT NOT NULL DEFAULT 'private'
        CHECK (visibility IN ('private', 'internal', 'public')),
    owner_id TEXT NOT NULL,
    company_id UUID,
    client_id TEXT,
    start_date TIMESTAMPTZ,
    due_date TIMESTAMPTZ,
    color TEXT,
    icon TEXT,
    labels JSONB NOT NULL DEFAULT '[]'::jsonb,
    custom_field_definitions JSONB NOT NULL DEFAULT '[]'::jsonb,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_projects_owner_id
    ON projects (owner_id);
CREATE INDEX IF NOT EXISTS idx_projects_status
    ON projects (status);
CREATE INDEX IF NOT EXISTS idx_projects_methodology
    ON projects (methodology);

-- ---------------------------------------------------------------------------
-- One membership of a user in a project, with a role.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_members (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL
        CHECK (role IN ('owner', 'manager', 'member', 'viewer')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_project_members_project_id
    ON project_members (project_id);
CREATE INDEX IF NOT EXISTS idx_project_members_user_id
    ON project_members (user_id);

-- ---------------------------------------------------------------------------
-- One board inside a project (a Kanban board, a Scrum board, a
-- Waterfall board, or any other board kind the methodology needs).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_boards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    wip_limit INTEGER CHECK (wip_limit IS NULL OR wip_limit > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_boards_project_id
    ON project_boards (project_id);

-- ---------------------------------------------------------------------------
-- One column on a board. `status_key` is the task status value this
-- column represents; `position` orders columns left to right.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_board_columns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    board_id UUID NOT NULL REFERENCES project_boards (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    status_key TEXT NOT NULL,
    wip_limit INTEGER CHECK (wip_limit IS NULL OR wip_limit > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_board_columns_board_id
    ON project_board_columns (board_id);

-- ---------------------------------------------------------------------------
-- One milestone: a dated checkpoint a project aims at.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_milestones (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    due_date TIMESTAMPTZ,
    reached BOOLEAN NOT NULL DEFAULT FALSE,
    reached_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_milestones_project_id
    ON project_milestones (project_id);

-- ---------------------------------------------------------------------------
-- One sprint: a time-boxed iteration with a goal, planned and actual
-- velocity, and notes from the standup, the review, and the
-- retrospective.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_sprints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    goal TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'active', 'completed')),
    start_date TIMESTAMPTZ,
    end_date TIMESTAMPTZ,
    velocity_planned DOUBLE PRECISION,
    velocity_actual DOUBLE PRECISION,
    standup_notes JSONB NOT NULL DEFAULT '[]'::jsonb,
    review_notes TEXT,
    retrospective_notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_sprints_project_id_status
    ON project_sprints (project_id, status);

-- ---------------------------------------------------------------------------
-- One phase: a Waterfall / PRINCE2 stage / DMAIC step / hybrid phase
-- with a gate decision at its end.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_phases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'waterfall'
        CHECK (kind IN ('waterfall', 'prince2_stage', 'dmaic', 'hybrid')),
    position INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'active', 'completed')),
    gate_status TEXT NOT NULL DEFAULT 'pending'
        CHECK (gate_status IN ('pending', 'approved', 'rejected')),
    gate_decision TEXT,
    start_date TIMESTAMPTZ,
    end_date TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_phases_project_id
    ON project_phases (project_id);

-- ---------------------------------------------------------------------------
-- One Shape Up cycle: a shaping / betting / building / cooldown
-- loop with an appetite in weeks and a pitch that won the bet.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_cycles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    appetite_weeks INTEGER NOT NULL DEFAULT 6
        CHECK (appetite_weeks BETWEEN 1 AND 12),
    status TEXT NOT NULL DEFAULT 'shaping'
        CHECK (status IN ('shaping', 'betting', 'building', 'cooldown', 'done')),
    started_at TIMESTAMPTZ,
    ends_at TIMESTAMPTZ,
    cooldown_weeks INTEGER NOT NULL DEFAULT 2,
    pitch TEXT,
    betting_decision TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_cycles_project_id
    ON project_cycles (project_id);

-- ---------------------------------------------------------------------------
-- One SAFe program increment: a longer planning horizon with
-- objectives, spanning several sprints.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_program_increments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'active', 'completed')),
    start_date TIMESTAMPTZ,
    end_date TIMESTAMPTZ,
    objectives JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_program_increments_project_id
    ON project_program_increments (project_id);

-- ---------------------------------------------------------------------------
-- One PRINCE2 business case: the justification for a project, with
-- benefits, costs, and risks, plus an exception report when the
-- project goes off its tolerances.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_business_cases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title TEXT NOT NULL,
    justification TEXT NOT NULL DEFAULT '',
    benefits JSONB NOT NULL DEFAULT '[]'::jsonb,
    costs JSONB NOT NULL DEFAULT '[]'::jsonb,
    risks JSONB NOT NULL DEFAULT '[]'::jsonb,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'approved', 'rejected')),
    exception_report TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_business_cases_project_id
    ON project_business_cases (project_id);

-- ---------------------------------------------------------------------------
-- One task: the unit of work. A task may be a story, bug, spike,
-- feature, enabler, or chore; it may belong to a sprint, a phase, a
-- cycle, a program increment, or a milestone, and it may carry the
-- methodology-specific fields (process group and knowledge area for
-- PMBOK, waste flag and pair partner for Lean and XP, DMAIC phase
-- for Six Sigma, appetite for Shape Up).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    board_id UUID REFERENCES project_boards (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    parent_id UUID REFERENCES project_tasks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    epic_id UUID REFERENCES project_tasks (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    task_type TEXT NOT NULL DEFAULT 'task'
        CHECK (task_type IN ('task', 'story', 'bug', 'spike', 'feature', 'enabler', 'chore')),
    status TEXT NOT NULL,
    priority TEXT NOT NULL DEFAULT 'normal'
        CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
    assignee_id TEXT,
    reporter_id TEXT NOT NULL,
    estimate_points DOUBLE PRECISION CHECK (estimate_points IS NULL OR estimate_points >= 0),
    estimate_hours DOUBLE PRECISION CHECK (estimate_hours IS NULL OR estimate_hours >= 0),
    labels JSONB NOT NULL DEFAULT '[]'::jsonb,
    custom_values JSONB NOT NULL DEFAULT '{}'::jsonb,
    start_date TIMESTAMPTZ,
    due_date TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    sprint_id UUID REFERENCES project_sprints (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    phase_id UUID REFERENCES project_phases (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    cycle_id UUID REFERENCES project_cycles (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    pi_id UUID REFERENCES project_program_increments (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    milestone_id UUID REFERENCES project_milestones (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    process_group TEXT,
    knowledge_area TEXT,
    waste_flag BOOLEAN,
    pair_partner_id TEXT,
    test_first BOOLEAN NOT NULL DEFAULT FALSE,
    dmaic_phase TEXT,
    appetite_weeks INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_tasks_project_id_status
    ON project_tasks (project_id, status);
CREATE INDEX IF NOT EXISTS idx_project_tasks_assignee_id
    ON project_tasks (assignee_id);
CREATE INDEX IF NOT EXISTS idx_project_tasks_sprint_id
    ON project_tasks (sprint_id);
CREATE INDEX IF NOT EXISTS idx_project_tasks_parent_id
    ON project_tasks (parent_id);

-- ---------------------------------------------------------------------------
-- One dependency between two tasks (finish-start, start-start,
-- finish-finish, start-finish, blocks, or related), with a lag in
-- days for the Critical Path methods.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_task_dependencies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES project_tasks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    depends_on_task_id UUID NOT NULL REFERENCES project_tasks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    kind TEXT NOT NULL DEFAULT 'finish_start'
        CHECK (kind IN ('finish_start', 'start_start', 'finish_finish', 'start_finish', 'blocks', 'related')),
    lag_days INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (task_id, depends_on_task_id, kind),
    CHECK (task_id <> depends_on_task_id)
);

CREATE INDEX IF NOT EXISTS idx_project_task_dependencies_task_id
    ON project_task_dependencies (task_id);
CREATE INDEX IF NOT EXISTS idx_project_task_dependencies_depends_on_task_id
    ON project_task_dependencies (depends_on_task_id);

-- ---------------------------------------------------------------------------
-- One comment on a task.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_task_comments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES project_tasks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    author_id TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_task_comments_task_id
    ON project_task_comments (task_id);

-- ---------------------------------------------------------------------------
-- One watcher on a task: a user who gets notified about it.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_task_watchers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES project_tasks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (task_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_project_task_watchers_task_id
    ON project_task_watchers (task_id);

-- ---------------------------------------------------------------------------
-- One file attached to a task. Only references are stored here; the
-- file itself lives in the storage module.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_task_attachments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES project_tasks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    target TEXT NOT NULL,
    key TEXT NOT NULL,
    stored_key TEXT NOT NULL,
    filename TEXT NOT NULL,
    content_type TEXT,
    size_bytes BIGINT,
    uploaded_by TEXT NOT NULL,
    uploaded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_task_attachments_task_id
    ON project_task_attachments (task_id);

-- ---------------------------------------------------------------------------
-- One time entry: hours logged against a task (and its project) by
-- a user, optionally billable and optionally already billed on an
-- invoice. A running timer is an entry with `timer_running` set
-- and no `ended_at` yet.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_time_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES project_tasks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    hours DOUBLE PRECISION NOT NULL DEFAULT 0
        CHECK (hours >= 0 AND hours <= 24),
    billable BOOLEAN NOT NULL DEFAULT FALSE,
    billed BOOLEAN NOT NULL DEFAULT FALSE,
    invoice_id TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ,
    timer_running BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_time_entries_project_id_billed
    ON project_time_entries (project_id, billed);
CREATE INDEX IF NOT EXISTS idx_project_time_entries_user_id
    ON project_time_entries (user_id);
CREATE INDEX IF NOT EXISTS idx_project_time_entries_task_id
    ON project_time_entries (task_id);

-- ---------------------------------------------------------------------------
-- One automation rule: when the trigger fires and the conditions
-- match, the actions run. `run_count` and `last_run_at` record the
-- rule's own history.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_automation_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    trigger TEXT NOT NULL
        CHECK (trigger IN ('task_created', 'status_changed', 'task_completed', 'due_soon', 'label_added')),
    conditions JSONB NOT NULL DEFAULT '{}'::jsonb,
    actions JSONB NOT NULL DEFAULT '[]'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    run_count BIGINT NOT NULL DEFAULT 0,
    last_run_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_automation_rules_project_id
    ON project_automation_rules (project_id);

-- ---------------------------------------------------------------------------
-- One project template: the seed boards, columns, statuses, phases,
-- and custom fields a methodology ships with.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    methodology TEXT NOT NULL,
    boards JSONB NOT NULL DEFAULT '[]'::jsonb,
    columns JSONB NOT NULL DEFAULT '[]'::jsonb,
    statuses JSONB NOT NULL DEFAULT '[]'::jsonb,
    phases JSONB NOT NULL DEFAULT '[]'::jsonb,
    custom_fields JSONB NOT NULL DEFAULT '[]'::jsonb,
    is_system BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_templates_methodology
    ON project_templates (methodology);

-- ---------------------------------------------------------------------------
-- One activity record: who did what to which project target, with
-- the details as JSON. Append-only; no `updated_at` is maintained
-- because an activity record is never edited after it is written.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS project_activity (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    actor_id TEXT NOT NULL,
    action TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id TEXT,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_project_activity_project_id_created_at
    ON project_activity (project_id, created_at);
