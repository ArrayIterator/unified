-- Roll back 2026-11-22-000067_web3, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.
-- `web3_anchors` references `web3_networks` with ON DELETE RESTRICT,
-- so it is dropped before `web3_networks` (and before the tables it
-- does not reference, to keep the order strictly reverse).

DROP TABLE IF EXISTS web3_settings CASCADE;
DROP TABLE IF EXISTS web3_balances CASCADE;
DROP TABLE IF EXISTS web3_wallets CASCADE;
DROP TABLE IF EXISTS web3_anchors CASCADE;
DROP TABLE IF EXISTS web3_transactions CASCADE;
DROP TABLE IF EXISTS web3_watched_addresses CASCADE;
DROP TABLE IF EXISTS web3_networks CASCADE;
