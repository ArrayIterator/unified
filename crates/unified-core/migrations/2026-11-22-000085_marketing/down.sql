-- Roll back 2026-11-22-000085_marketing, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.
-- Suppressions and the consent cache live only in the in-memory
-- stores of `src/modules/marketing.rs` and have no tables here;
-- audience snapshots are likewise in-memory only.

DROP TABLE IF EXISTS marketing_sends;
DROP TABLE IF EXISTS marketing_score_rules;
DROP TABLE IF EXISTS marketing_scores;
DROP TABLE IF EXISTS marketing_enrollments;
DROP TABLE IF EXISTS marketing_sequence_steps;
DROP TABLE IF EXISTS marketing_sequences;
DROP TABLE IF EXISTS marketing_segment_size_history;
DROP TABLE IF EXISTS marketing_segments;
DROP TABLE IF EXISTS marketing_campaign_variants;
DROP TABLE IF EXISTS marketing_campaigns;
