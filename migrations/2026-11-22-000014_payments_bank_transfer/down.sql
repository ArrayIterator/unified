-- Roll back 2026-11-22-000014_payments_bank_transfer, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.
--
-- Note: `payment_transactions` holds the foreign key to
-- `gateway_subscriptions`, so transactions go before
-- subscriptions; `split_parts` holds the foreign key to
-- `payment_transactions`, so parts go before transactions.

DROP TABLE IF EXISTS bank_transfer_settings;
DROP TABLE IF EXISTS bank_transfer_verification_audit;
DROP TABLE IF EXISTS bank_transfer_verifications;
DROP TABLE IF EXISTS bank_accounts;
DROP TABLE IF EXISTS split_parts;
DROP TABLE IF EXISTS split_bills;
DROP TABLE IF EXISTS payment_transactions;
DROP TABLE IF EXISTS gateway_subscriptions;
DROP TABLE IF EXISTS payment_methods;
DROP TABLE IF EXISTS payment_intents;
DROP TABLE IF EXISTS payment_connections;
