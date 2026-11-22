-- Roll back 2026-11-22-000012_logs_billing, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.

DROP TABLE IF EXISTS invoice_sequences;
DROP TABLE IF EXISTS billing_settings;
DROP TABLE IF EXISTS credit_notes;
DROP TABLE IF EXISTS invoice_payments;
DROP TABLE IF EXISTS invoice_items;
DROP TABLE IF EXISTS invoices;
DROP TABLE IF EXISTS logs_settings;
DROP TABLE IF EXISTS app_logs;
