-- Approval tables for the Unified Pentagonal Project
-- (`src/modules/approvals.rs`): data-driven approval workflows for
-- any record in any module.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; the persistence layer is not wired yet):
--   - `approval_definitions.module` / `record_type` and
--     `approval_requests.module` / `record_type` / `record_id` are
--     plain text: a request is polymorphic and there is deliberately
--     NO foreign key into another module's tables. The owning module
--     owns its records; this module only points at them.
--   - `approval_requests.requester_id`,
--     `approval_decisions.approver_id` / `acting_for`, and
--     `approval_delegations.delegator_id` / `delegate_id` are plain
--     VARCHAR user ids: the module identifies callers by the
--     `X-User-Id` header and keeps user ids as strings, exactly like
--     `store_orders.user_id` in migration 000002. No foreign key to
--     `users` is stored here.
--   - `approval_steps.approvers` is JSONB: an array of
--     `{"kind": "user"|"role"|"manager", "ref_value": "..."}` slots.
--     Approver kinds, step modes, and escalation actions are
--     validated in code against the module's data-driven registries;
--     the CHECKs below mirror the same value sets so the database
--     rejects anything the code would also reject.
--   - `approval_requests.status` is `requested`, `in_progress`,
--     `approved`, `rejected`, `changes_requested`, `expired`, or
--     `cancelled` (the request state machine).
--   - `approval_requests.amount_minor` is an integer in minor units
--     (never a float); `currency` is an ISO 4217 code.
--   - `approval_requests.round` is the decision round, incremented
--     on every resubmit. Only decisions from the current round count
--     towards step completion; earlier rounds stay visible in
--     history.
--   - `approval_decisions` is append-only: the trigger at the end of
--     this file rejects UPDATE and DELETE on it. Decisions are never
--     edited or deleted.
--   - `approval_delegations` rows are soft-revoked (`is_active =
--     false`) rather than deleted, so past decisions that recorded
--     `acting_for` keep their meaning.
--   - At most one ACTIVE definition may exist per
--     (`module`, `record_type`) pair; that rule is enforced in code
--     (a partial unique index cannot express "one active row"
--     together with the module's soft-delete-free history, and the
--     definition is resolved by newest-first as a safety net), so
--     there is no partial unique index here.
--   - Money does not move in this migration: approval only records
--     the optional amount for display and filtering.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One approval definition: for records of (module, record_type),
-- which steps apply.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS approval_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    module VARCHAR(128) NOT NULL,
    record_type VARCHAR(128) NOT NULL,
    name VARCHAR(200) NOT NULL,
    description TEXT,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    timeout_hours INTEGER CHECK (timeout_hours IS NULL OR (timeout_hours >= 1 AND timeout_hours <= 720)),
    escalation_action VARCHAR(32) NOT NULL DEFAULT 'notify'
        CHECK (escalation_action IN ('notify', 'notify_and_reassign', 'none')),
    escalate_to JSONB,
    escalation_extension_hours INTEGER CHECK (escalation_extension_hours IS NULL OR (escalation_extension_hours >= 1 AND escalation_extension_hours <= 720)),
    company_id VARCHAR(128),
    created_by VARCHAR(128) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_approval_definitions_module_record
    ON approval_definitions (module, record_type, is_active);
CREATE INDEX IF NOT EXISTS idx_approval_definitions_company
    ON approval_definitions (company_id);

-- ---------------------------------------------------------------------------
-- One ordered step of a definition.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS approval_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    definition_id UUID NOT NULL REFERENCES approval_definitions (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 1),
    name VARCHAR(200) NOT NULL,
    mode VARCHAR(8) NOT NULL DEFAULT 'any'
        CHECK (mode IN ('all', 'any')),
    approvers JSONB NOT NULL DEFAULT '[]'::jsonb,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (definition_id, position)
);

CREATE INDEX IF NOT EXISTS idx_approval_steps_definition
    ON approval_steps (definition_id, position);

-- ---------------------------------------------------------------------------
-- One approval request for one record in one module.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS approval_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    definition_id UUID NOT NULL REFERENCES approval_definitions (id) ON DELETE RESTRICT,
    module VARCHAR(128) NOT NULL,
    record_type VARCHAR(128) NOT NULL,
    record_id VARCHAR(256) NOT NULL,
    requester_id VARCHAR(128) NOT NULL,
    summary VARCHAR(1000) NOT NULL,
    amount_minor BIGINT CHECK (amount_minor IS NULL OR amount_minor >= 0),
    currency VARCHAR(8),
    status VARCHAR(32) NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'in_progress', 'approved', 'rejected', 'changes_requested', 'expired', 'cancelled')),
    current_step_index INTEGER NOT NULL DEFAULT 0 CHECK (current_step_index >= 0),
    round INTEGER NOT NULL DEFAULT 1 CHECK (round >= 1),
    escalation_count INTEGER NOT NULL DEFAULT 0 CHECK (escalation_count >= 0),
    last_escalated_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    decided_at TIMESTAMPTZ,
    decided_by VARCHAR(128),
    company_id VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_approval_requests_record
    ON approval_requests (module, record_type, record_id);
CREATE INDEX IF NOT EXISTS idx_approval_requests_requester
    ON approval_requests (requester_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_approval_requests_status_expiry
    ON approval_requests (status, expires_at);
CREATE INDEX IF NOT EXISTS idx_approval_requests_definition
    ON approval_requests (definition_id);

-- ---------------------------------------------------------------------------
-- One decision. Append-only: never edited, never deleted (see the
-- trigger below).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS approval_decisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id UUID NOT NULL REFERENCES approval_requests (id) ON DELETE CASCADE,
    step_index INTEGER NOT NULL CHECK (step_index >= 0),
    round INTEGER NOT NULL CHECK (round >= 1),
    approver_id VARCHAR(128) NOT NULL,
    acting_for VARCHAR(128),
    slot_kind VARCHAR(16) NOT NULL
        CHECK (slot_kind IN ('user', 'role', 'manager')),
    slot_ref VARCHAR(128) NOT NULL DEFAULT '',
    decision VARCHAR(16) NOT NULL
        CHECK (decision IN ('approve', 'reject', 'request_changes')),
    comment TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_approval_decisions_request
    ON approval_decisions (request_id, created_at);

-- ---------------------------------------------------------------------------
-- A temporary delegation: delegate_id may decide in place of
-- delegator_id between starts_at and ends_at.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS approval_delegations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    delegator_id VARCHAR(128) NOT NULL,
    delegate_id VARCHAR(128) NOT NULL,
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by VARCHAR(128) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at),
    CHECK (delegator_id <> delegate_id)
);

CREATE INDEX IF NOT EXISTS idx_approval_delegations_delegator
    ON approval_delegations (delegator_id, is_active);
CREATE INDEX IF NOT EXISTS idx_approval_delegations_delegate
    ON approval_delegations (delegate_id, is_active);

-- ---------------------------------------------------------------------------
-- One escalation event on a request.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS approval_escalations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id UUID NOT NULL REFERENCES approval_requests (id) ON DELETE CASCADE,
    step_index INTEGER NOT NULL CHECK (step_index >= 0),
    reason VARCHAR(500) NOT NULL,
    action VARCHAR(32) NOT NULL
        CHECK (action IN ('notify', 'notify_and_reassign', 'none')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_approval_escalations_request
    ON approval_escalations (request_id, created_at);

-- ---------------------------------------------------------------------------
-- Append-only enforcement for approval_decisions.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION forbid_approval_decision_mutation()
RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'approval_decisions is append-only: decisions are never edited or deleted';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_approval_decisions_no_update ON approval_decisions;
CREATE TRIGGER trg_approval_decisions_no_update
    BEFORE UPDATE ON approval_decisions
    FOR EACH ROW EXECUTE FUNCTION forbid_approval_decision_mutation();

DROP TRIGGER IF EXISTS trg_approval_decisions_no_delete ON approval_decisions;
CREATE TRIGGER trg_approval_decisions_no_delete
    BEFORE DELETE ON approval_decisions
    FOR EACH ROW EXECUTE FUNCTION forbid_approval_decision_mutation();
