-- Roll back 2026-11-22-000036_ai_allowlists, in reverse dependency
-- order. The 000030_ai tables keep their rows; only the columns this
-- migration added to `ai_usage_records` are dropped. The core tables
-- from 2026-11-22-000001_core are untouched.

ALTER TABLE ai_usage_records
    DROP COLUMN IF EXISTS used_personal_key,
    DROP COLUMN IF EXISTS denial_reason,
    DROP COLUMN IF EXISTS company_id,
    DROP COLUMN IF EXISTS user_id;

DROP TABLE IF EXISTS ai_user_mcp_servers;
DROP TABLE IF EXISTS ai_personal_keys;
DROP TABLE IF EXISTS ai_user_policies;
DROP TABLE IF EXISTS ai_company_policies;
