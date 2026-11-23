-- Referral module (`src/modules/referral/`): drop the
-- persistence tables in reverse dependency order.
-- The in-memory DashMap store is authoritative; dropping
-- these tables only removes the persistence targets.

DROP TABLE IF EXISTS referral_audit;
DROP TABLE IF EXISTS referral_employees;
DROP TABLE IF EXISTS referral_anti_gaming_rules;
DROP TABLE IF EXISTS referral_rewards;
DROP TABLE IF EXISTS referral_stage_history;
DROP TABLE IF EXISTS referral_referrals;
DROP TABLE IF EXISTS referral_reward_rules;
DROP TABLE IF EXISTS referral_pipeline_stages;
DROP TABLE IF EXISTS referral_links;
DROP TABLE IF EXISTS referral_job_eligibility;
