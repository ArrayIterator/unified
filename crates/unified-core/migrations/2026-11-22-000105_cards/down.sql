-- Roll back 2026-11-22-000105_cards, in reverse dependency
-- order. No other module's tables are touched.
--
-- CREATE TABLE must equal DROP TABLE: the two tables created in
-- up.sql (card_custom_rules, card_validation_daily_counts) are
-- exactly the two tables dropped here.

DROP TABLE IF EXISTS card_validation_daily_counts;
DROP TABLE IF EXISTS card_custom_rules;
