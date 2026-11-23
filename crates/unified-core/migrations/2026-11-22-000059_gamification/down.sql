-- Roll back 2026-11-22-000059_gamification, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS gamification_reward_requests;
DROP TABLE IF EXISTS gamification_rewards;
DROP TABLE IF EXISTS gamification_seasons;
DROP TABLE IF EXISTS gamification_awards;
DROP TABLE IF EXISTS gamification_badges;
DROP TABLE IF EXISTS gamification_points_ledger;
DROP TABLE IF EXISTS gamification_rules;
