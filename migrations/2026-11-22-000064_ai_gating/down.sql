-- Roll back 2026-11-22-000064_ai_gating: drop the global AI
-- settings table and the gate-switch columns on the batch-2
-- allowlist tables. The allowlist tables themselves (000036) are
-- untouched.
DROP TABLE IF EXISTS ai_global_settings;

ALTER TABLE ai_user_policies
    DROP COLUMN IF EXISTS disabled_features,
    DROP COLUMN IF EXISTS ai_enabled;

ALTER TABLE ai_company_policies
    DROP COLUMN IF EXISTS disabled_features,
    DROP COLUMN IF EXISTS ai_enabled;
