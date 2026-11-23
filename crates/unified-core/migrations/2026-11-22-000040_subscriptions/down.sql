-- Roll back 2026-11-22-000040_subscriptions, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS dunning_attempts;
DROP TABLE IF EXISTS subscription_events;
DROP TABLE IF EXISTS subscriptions;
