-- Referral module (`src/modules/referral/`):
-- Employee referral: recruitment jobs marked
-- referral-eligible with a per-job reward rule override or
-- the tenant default rule, unique token-based shareable
-- referral links per (employee, job) with click counts
-- only, referrals with one entry per (candidate email,
-- job), a fully data-driven pipeline (admin-defined stages
-- with allowed transitions, audited moves), reward rules as
-- data (amount in integer minor units, payout trigger
-- stage, retention days, eligibility conditions) with
-- rewards that become payable through events for billing /
-- HR to pay out, anti-gaming rules as data (self-referral,
-- same-candidate cooldown, per-period cap,
-- active-employee check), and an append-only audit trail.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000119_esg:
-- the in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz.
--     `job_id` / `link_id` / `referral_id` / `stage_id` /
--     `rule_id` / `trigger_stage_id` / `reward_rule_id`
--     reference referral or recruitment rows by id with NO
--     foreign key, so this module must not assume any other
--     module's tables or migration order. `referrer_user_id` /
--     `created_by` / `moved_by` / `paid_by` / `actor` are
--     plain TEXT with NO foreign key on purpose: users live
--     in the users module and this module must not assume
--     its table or migration order. `job_id` is a plain
--     reference into the recruitment module by id only, also
--     with NO foreign key: the recruitment module emits no
--     job-existence event, so there is no existence check
--     here.
--   - `tenant_id` is plain TEXT on every row: referral data is
--     tenant business data and every query filters by it.
--     A row from another tenant reads as 404, never 403.
--   - Nothing is hard-coded: pipeline stages (including
--     their order, terminal / hired / rejected flags, and
--     allowed transitions), reward rules (including their
--     amount, trigger stage, retention days, and eligibility
--     conditions), job eligibility overrides, and
--     anti-gaming rules (including their thresholds and
--     enforcement) are all data rows. The only fixed
--     vocabulary is the reward status (`pending`,
--     `eligible`, `paid`, `voided`), the four anti-gaming
--     rule kinds (`block_self_referral`,
--     `same_candidate_cooldown`, `max_referrals_per_period`,
--     `require_active_employee`), and the two enforcement
--     modes (`block`, `flag`) — those are the shape of the
--     module, not its content.
--   - Money is integer minor units: `amount_minor` is a
--     BIGINT count of the smallest currency unit (e.g.
--     cents) with a three-letter ISO currency code, and
--     `f64` is never used for a stored or returned amount
--     anywhere in this module.
--   - Referral link tokens are stored as SHA-256 hex
--     digests only (`referral_links.token_hash`); the raw
--     token is returned exactly once at creation / rotation
--     and is never stored. Click tracking is counts only
--     (`click_count`, `last_clicked_at`): no IP address,
--     user agent, or fingerprint is stored.
--   - Stage history is append-only: every referral creation
--     and every stage move appends one
--     `referral_stage_history` row with the previous stage,
--     the new stage, the actor, and the reason, so the full
--     pipeline history of a referral is preserved even
--     though the referral row itself only holds the current
--     stage. Audit rows are likewise never updated or
--     deleted through the API.
--   - Rewards snapshot their rule at creation (`rule_name`,
--     `amount_minor`, `currency`): editing or deleting a
--     reward rule never rewrites a reward already owed.
--     Payout is never done here — `referral.reward.payable`
--     is the handoff to billing / HR, and `mark-paid` only
--     records that a payout happened elsewhere.
--   - The known-employee registry (`referral_employees`)
--     is a cache built from `hr.employee_created` events,
--     which carry no tenant: `tenant_id` is empty on those
--     rows. Presence in the registry means the user is a
--     known employee; absence means unknown, never "not an
--     employee". The active-employee check is therefore
--     recorded as `unverified` whenever the registry has no
--     record, and it never blocks a submission on its own.
--   - Deleting a pipeline stage, a reward rule, a link, or
--     a referral never deletes the rows that reference it:
--     referrals keep their stage snapshot, rewards keep
--     their rule snapshot, and history and audit rows stay
--     on record. A referral with a paid reward cannot be
--     deleted at all.
--   - Unique indexes enforce the module invariants in the
--     database too: one eligibility row per (tenant, job),
--     one active link per (employee, job), one token hash
--     per link, one stage slug per tenant, one referral per
--     (tenant, candidate email, job), one reward per
--     (referral, rule), and one known-employee row per user
--     id.
--   - This migration seeds no rows: a tenant with no stage
--     rows cannot accept referrals yet, and a tenant with
--     no reward rule rows owes no rewards yet.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Job eligibility: one row per recruitment job a tenant has
-- explicitly marked. A job with no row is eligible by
-- default; a row with `eligible` false blocks new referrals
-- for it. `reward_rule_id` is an optional per-job override
-- of the tenant default reward rule (plain reference, no
-- foreign key — deleting the rule clears the override).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_job_eligibility (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    job_id UUID NOT NULL,
    eligible BOOLEAN NOT NULL DEFAULT TRUE,
    reward_rule_id UUID,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_referral_eligibility_tenant_job ON referral_job_eligibility (tenant_id, job_id);
CREATE INDEX IF NOT EXISTS idx_referral_eligibility_tenant_eligible ON referral_job_eligibility (tenant_id, eligible, created_at DESC);

-- ---------------------------------------------------------------------------
-- Referral links: one shareable link per (employee, job).
-- Only the SHA-256 hash of the token is stored
-- (`token_hash`, 64 hex characters); the raw token is
-- returned exactly once at creation / rotation. Clicks are
-- counted only — no IP address, user agent, or fingerprint
-- is stored. `active` false is a revoked link: the token
-- stops working, the row and its click count are kept.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    job_id UUID NOT NULL,
    referrer_user_id TEXT NOT NULL CHECK (char_length(referrer_user_id) BETWEEN 1 AND 200),
    token_hash TEXT NOT NULL CHECK (char_length(token_hash) = 64),
    click_count BIGINT NOT NULL DEFAULT 0 CHECK (click_count >= 0),
    last_clicked_at TIMESTAMPTZ,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_referral_links_tenant_employee_job ON referral_links (tenant_id, referrer_user_id, job_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_referral_links_token_hash ON referral_links (token_hash);
CREATE INDEX IF NOT EXISTS idx_referral_links_tenant_job ON referral_links (tenant_id, job_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Pipeline stages: one admin-defined stage of the referral
-- pipeline. `slug` is unique per tenant and is the stable
-- handle transitions use; `sort_order` is the pipeline
-- order; `is_terminal` stages have no outgoing transitions;
-- `is_hired_stage` marks the stage whose entry creates the
-- reward; `is_rejected_stage` marks the stage whose entry
-- voids open rewards. `allowed_next_stage_ids` is a JSON
-- array of stage ids this stage may move to (plain
-- references, no foreign key — deleting a stage prunes it
-- from other stages' lists).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_pipeline_stages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_terminal BOOLEAN NOT NULL DEFAULT FALSE,
    is_hired_stage BOOLEAN NOT NULL DEFAULT FALSE,
    is_rejected_stage BOOLEAN NOT NULL DEFAULT FALSE,
    allowed_next_stage_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (NOT (is_hired_stage AND is_rejected_stage))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_referral_stages_tenant_slug ON referral_pipeline_stages (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_referral_stages_tenant_order ON referral_pipeline_stages (tenant_id, sort_order ASC);

-- ---------------------------------------------------------------------------
-- Reward rules: one admin-defined payout rule. `amount_minor`
-- is the payout in integer minor units with its three-letter
-- ISO currency code; `trigger_stage_id` is the pipeline
-- stage whose entry creates the reward (NULL means the hired
-- stage itself); `retention_days` is how long after reaching
-- the trigger stage the reward becomes payable (`0` means
-- payable at once); `eligibility_conditions` is a JSON array
-- of free-text conditions; `is_default` marks the tenant's
-- default rule (at most one per tenant — enforced by the
-- application, not the database, so a tenant can briefly
-- hold none while switching defaults).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_reward_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency TEXT NOT NULL CHECK (char_length(currency) = 3),
    trigger_stage_id UUID,
    retention_days BIGINT NOT NULL DEFAULT 0 CHECK (retention_days >= 0),
    eligibility_conditions JSONB NOT NULL DEFAULT '[]'::jsonb,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_referral_rules_tenant_active ON referral_reward_rules (tenant_id, active, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_referral_rules_tenant_default ON referral_reward_rules (tenant_id, is_default, created_at DESC);

-- ---------------------------------------------------------------------------
-- Referrals: one referral of one candidate for one job.
-- `candidate_email` is stored normalized (lowercase); one
-- referral per (tenant, candidate email, job) is enforced
-- by a unique index. `referrer_user_id` is the employee who
-- referred the candidate (plain TEXT, no foreign key);
-- `referrer_verified` records whether the referrer was
-- confirmed as an active employee through the HR registry,
-- and `verification_note` explains the outcome, including
-- the `unverified` case. `stage_id` / `stage_slug` are the
-- current pipeline stage (the slug is a snapshot, updated
-- when a stage is renamed); `link_id` is the referral link
-- used, when one was used; `source` names how the referral
-- arrived (`link` or `manual`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_referrals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    job_id UUID NOT NULL,
    referrer_user_id TEXT NOT NULL CHECK (char_length(referrer_user_id) BETWEEN 1 AND 200),
    referrer_verified BOOLEAN NOT NULL DEFAULT FALSE,
    verification_note TEXT NOT NULL DEFAULT '' CHECK (char_length(verification_note) BETWEEN 0 AND 5000),
    candidate_name TEXT NOT NULL CHECK (char_length(candidate_name) BETWEEN 1 AND 200),
    candidate_email TEXT NOT NULL CHECK (char_length(candidate_email) BETWEEN 1 AND 254),
    candidate_phone TEXT CHECK (candidate_phone IS NULL OR char_length(candidate_phone) BETWEEN 1 AND 64),
    link_id UUID,
    stage_id UUID NOT NULL,
    stage_slug TEXT NOT NULL CHECK (char_length(stage_slug) BETWEEN 1 AND 64),
    source TEXT CHECK (source IS NULL OR char_length(source) BETWEEN 1 AND 64),
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_referral_referrals_tenant_email_job ON referral_referrals (tenant_id, candidate_email, job_id);
CREATE INDEX IF NOT EXISTS idx_referral_referrals_tenant_job ON referral_referrals (tenant_id, job_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_referral_referrals_tenant_referrer ON referral_referrals (tenant_id, referrer_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_referral_referrals_tenant_stage ON referral_referrals (tenant_id, stage_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Stage history: one row per referral creation and every
-- stage move. `from_stage_id` / `from_stage_slug` are NULL
-- on creation (the referral starts in its initial stage).
-- History rows are never updated or deleted.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_stage_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    referral_id UUID NOT NULL,
    job_id UUID NOT NULL,
    from_stage_id UUID,
    from_stage_slug TEXT CHECK (from_stage_slug IS NULL OR char_length(from_stage_slug) BETWEEN 1 AND 64),
    to_stage_id UUID NOT NULL,
    to_stage_slug TEXT NOT NULL CHECK (char_length(to_stage_slug) BETWEEN 1 AND 64),
    moved_by TEXT NOT NULL CHECK (char_length(moved_by) BETWEEN 1 AND 200),
    reason TEXT CHECK (reason IS NULL OR char_length(reason) BETWEEN 1 AND 1000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_referral_history_referral ON referral_stage_history (referral_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_referral_history_tenant_job ON referral_stage_history (tenant_id, job_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Rewards: one reward owed for a referral under one rule.
-- `rule_name`, `amount_minor`, and `currency` are snapshotted
-- from the rule at creation, so editing or deleting the rule
-- never rewrites a reward already owed. `status` is
-- `pending`, `eligible`, `paid`, or `voided`; `eligible_at`
-- is when the retention period elapses; `paid_at` /
-- `paid_by` record a payout made elsewhere (this module
-- never moves money); `void_reason` records why an open
-- reward was voided. One reward per (referral, rule).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_rewards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    referral_id UUID NOT NULL,
    job_id UUID NOT NULL,
    referrer_user_id TEXT NOT NULL CHECK (char_length(referrer_user_id) BETWEEN 1 AND 200),
    rule_id UUID NOT NULL,
    rule_name TEXT NOT NULL CHECK (char_length(rule_name) BETWEEN 1 AND 200),
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency TEXT NOT NULL CHECK (char_length(currency) = 3),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'eligible', 'paid', 'voided')),
    eligible_at TIMESTAMPTZ,
    paid_at TIMESTAMPTZ,
    paid_by TEXT CHECK (paid_by IS NULL OR char_length(paid_by) BETWEEN 1 AND 200),
    void_reason TEXT CHECK (void_reason IS NULL OR char_length(void_reason) BETWEEN 1 AND 1000),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_referral_rewards_referral_rule ON referral_rewards (referral_id, rule_id);
CREATE INDEX IF NOT EXISTS idx_referral_rewards_tenant_status ON referral_rewards (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_referral_rewards_tenant_referrer ON referral_rewards (tenant_id, referrer_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_referral_rewards_tenant_job ON referral_rewards (tenant_id, job_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Anti-gaming rules: one admin-defined check. `rule_kind`
-- is `block_self_referral`, `same_candidate_cooldown`,
-- `max_referrals_per_period`, or `require_active_employee`;
-- `limit_count` plus `window_days` parameter the per-period
-- cap, `cooldown_days` parameter the same-candidate
-- cooldown; `enforcement` is `block` (reject the submission
-- with 409) or `flag` (record the violation on the referral
-- row and let it through for review). Checks that cannot be
-- evaluated from the data this module has never block — they
-- are recorded as `unverified` on the referral row.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_anti_gaming_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    rule_kind TEXT NOT NULL CHECK (rule_kind IN ('block_self_referral', 'same_candidate_cooldown', 'max_referrals_per_period', 'require_active_employee')),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    limit_count BIGINT CHECK (limit_count IS NULL OR limit_count >= 0),
    window_days BIGINT CHECK (window_days IS NULL OR window_days >= 0),
    cooldown_days BIGINT CHECK (cooldown_days IS NULL OR cooldown_days >= 0),
    enforcement TEXT NOT NULL DEFAULT 'flag' CHECK (enforcement IN ('block', 'flag')),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_referral_gaming_tenant_kind ON referral_anti_gaming_rules (tenant_id, rule_kind, created_at DESC);

-- ---------------------------------------------------------------------------
-- Known employees: a cache built from `hr.employee_created`
-- events (`{employee_id, user_id, department_id}`), used
-- only for the active-employee check. The event carries no
-- tenant, so `tenant_id` is empty on these rows and the
-- lookup is by user id. One row per user id; deleting the
-- user removes the row. Absence means unknown, never "not
-- an employee".
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_employees (
    user_id TEXT PRIMARY KEY CHECK (char_length(user_id) BETWEEN 1 AND 200),
    employee_id TEXT NOT NULL CHECK (char_length(employee_id) BETWEEN 1 AND 200),
    tenant_id TEXT NOT NULL DEFAULT '' CHECK (char_length(tenant_id) BETWEEN 0 AND 200),
    department_id TEXT CHECK (department_id IS NULL OR char_length(department_id) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Audit trail: one row per change to an eligibility row, a
-- link, a stage, a reward rule, a referral, a reward, or an
-- anti-gaming rule (creates, updates, deletes, stage moves,
-- rotations, revocations, mark-paid, voids). `entity_type`
-- names the row kind; `entity_id` is the row id as TEXT;
-- `action` names the change; `before` / `after` are the
-- serialized rows (or NULL for creates / deletes). Audit rows
-- are never updated or deleted through the API.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS referral_audit (
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
CREATE INDEX IF NOT EXISTS idx_referral_audit_tenant_created ON referral_audit (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_referral_audit_tenant_entity ON referral_audit (tenant_id, entity_type, entity_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_referral_audit_tenant_action ON referral_audit (tenant_id, action, created_at DESC);
