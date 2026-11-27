-- Gamification tables for the Unified Pentagonal Project
-- (`src/modules/gamification.rs`): points rules, the points
-- ledger, badge definitions, badge awards, seasons, rewards, and
-- reward redemption requests.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/gamification.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - STAFF LEDGER SEPARATION: these points are STAFF
--     recognition points. They are a SEPARATE ledger from
--     customer loyalty points (the loyalty module owns the
--     customer ledger) and are never mixed: no row here
--     references loyalty, and no loyalty row references this
--     ledger.
--   - `gamification_rules.action` is one of the known actions
--     (`ticket_closed`, `sale_completed`, `review_received`,
--     `community_help`, `post_created`, `topic_created`,
--     `survey_completed`, `manual_adjustment`). `company_id`
--     NULL means the rule applies to every company; a
--     company-specific rule wins over a global one for the same
--     action (enforced in the module). `daily_cap` caps the
--     points one user can earn from this rule per calendar day
--     (UTC); NULL means no cap.
--   - `gamification_points_ledger` is the ONLY source of truth
--     for balances: a user's balance is the sum of their entries.
--     There is no balance column anywhere. `points` may be
--     negative (adjustments and reward deductions).
--     `balance_after` is the running balance recorded at write
--     time for audit convenience; it is never used as the
--     balance itself. `user_id`, `company_id`, and
--     `department_id` are plain VARCHAR references (no foreign
--     keys: the users / company / departments modules own their
--     rows); the module identifies callers by the `X-User-Id`
--     header, exactly like `store_orders.user_id` in migration
--     000002.
--   - `gamification_badges.criteria_action` names the action
--     whose count is compared against `criteria_threshold`.
--   - `gamification_awards` has one row per (badge, user) at
--     most (unique index); re-earning the same badge is a no-op
--     in the module.
--   - `gamification_seasons.status` is `upcoming`, `active`, or
--     `ended`. At most one season is `active` per company
--     (enforced in the module on start); the partial unique
--     index below enforces it in the database for non-NULL
--     companies only - a NULL company is the global scope, and
--     NULLs are distinct in a unique index, so the one-active
--     rule for global seasons is enforced by the module alone.
--   - `gamification_reward_requests.status` is `requested`,
--     `approved`, `rejected`, or `fulfilled`. Points are
--     deducted exactly once, on approval, as a negative ledger
--     entry with action `reward_redemption`.

CREATE TABLE IF NOT EXISTS gamification_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    action VARCHAR(64) NOT NULL CHECK (action IN ('ticket_closed', 'sale_completed', 'review_received', 'community_help', 'post_created', 'topic_created', 'survey_completed', 'manual_adjustment')),
    points BIGINT NOT NULL CHECK (points >= 0 AND points <= 1000000),
    company_id VARCHAR(200),
    daily_cap BIGINT CHECK (daily_cap IS NULL OR (daily_cap >= 0 AND daily_cap <= 1000000)),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS gamification_rules_action_idx ON gamification_rules (action);
CREATE INDEX IF NOT EXISTS gamification_rules_company_idx ON gamification_rules (company_id);
CREATE INDEX IF NOT EXISTS gamification_rules_active_idx ON gamification_rules (active);

CREATE TABLE IF NOT EXISTS gamification_points_ledger (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR(255) NOT NULL,
    company_id VARCHAR(200),
    department_id VARCHAR(200),
    rule_id UUID REFERENCES gamification_rules (id) ON DELETE SET NULL,
    action VARCHAR(64) NOT NULL,
    points BIGINT NOT NULL CHECK (points >= -1000000 AND points <= 1000000),
    balance_after BIGINT NOT NULL,
    season_id UUID,
    reason VARCHAR(2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS gamification_points_ledger_user_idx ON gamification_points_ledger (user_id);
CREATE INDEX IF NOT EXISTS gamification_points_ledger_action_idx ON gamification_points_ledger (action);
CREATE INDEX IF NOT EXISTS gamification_points_ledger_company_idx ON gamification_points_ledger (company_id);
CREATE INDEX IF NOT EXISTS gamification_points_ledger_department_idx ON gamification_points_ledger (department_id);
CREATE INDEX IF NOT EXISTS gamification_points_ledger_season_idx ON gamification_points_ledger (season_id);
CREATE INDEX IF NOT EXISTS gamification_points_ledger_rule_idx ON gamification_points_ledger (rule_id);
CREATE INDEX IF NOT EXISTS gamification_points_ledger_created_at_idx ON gamification_points_ledger (created_at);

CREATE TABLE IF NOT EXISTS gamification_badges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT NOT NULL,
    criteria_action VARCHAR(64) NOT NULL CHECK (criteria_action IN ('ticket_closed', 'sale_completed', 'review_received', 'community_help', 'post_created', 'topic_created', 'survey_completed', 'manual_adjustment')),
    criteria_threshold BIGINT NOT NULL CHECK (criteria_threshold >= 1 AND criteria_threshold <= 1000000),
    icon VARCHAR(200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS gamification_badges_action_idx ON gamification_badges (criteria_action);

CREATE TABLE IF NOT EXISTS gamification_awards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    badge_id UUID NOT NULL REFERENCES gamification_badges (id) ON DELETE RESTRICT,
    user_id VARCHAR(255) NOT NULL,
    reason VARCHAR(2000) NOT NULL,
    awarded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS gamification_awards_badge_user_idx ON gamification_awards (badge_id, user_id);
CREATE INDEX IF NOT EXISTS gamification_awards_user_idx ON gamification_awards (user_id);
CREATE INDEX IF NOT EXISTS gamification_awards_awarded_at_idx ON gamification_awards (awarded_at);

CREATE TABLE IF NOT EXISTS gamification_seasons (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    company_id VARCHAR(200),
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'upcoming' CHECK (status IN ('upcoming', 'active', 'ended')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS gamification_seasons_status_idx ON gamification_seasons (status);
CREATE INDEX IF NOT EXISTS gamification_seasons_company_idx ON gamification_seasons (company_id);
CREATE UNIQUE INDEX IF NOT EXISTS gamification_seasons_one_active_per_company_idx ON gamification_seasons (company_id) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS gamification_rewards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT NOT NULL,
    points_cost BIGINT NOT NULL CHECK (points_cost >= 1 AND points_cost <= 1000000),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS gamification_rewards_active_idx ON gamification_rewards (active);

CREATE TABLE IF NOT EXISTS gamification_reward_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    reward_id UUID NOT NULL REFERENCES gamification_rewards (id) ON DELETE RESTRICT,
    user_id VARCHAR(255) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'requested' CHECK (status IN ('requested', 'approved', 'rejected', 'fulfilled')),
    points_spent BIGINT NOT NULL CHECK (points_spent >= 1),
    decided_by VARCHAR(255),
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS gamification_reward_requests_reward_idx ON gamification_reward_requests (reward_id);
CREATE INDEX IF NOT EXISTS gamification_reward_requests_user_idx ON gamification_reward_requests (user_id);
CREATE INDEX IF NOT EXISTS gamification_reward_requests_status_idx ON gamification_reward_requests (status);
CREATE INDEX IF NOT EXISTS gamification_reward_requests_created_at_idx ON gamification_reward_requests (created_at);
