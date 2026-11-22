-- Roll back 2026-11-22-000106_calculator, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS calculator_currency_rates;
DROP TABLE IF EXISTS calculator_history;
DROP TABLE IF EXISTS calculator_reports;
DROP TABLE IF EXISTS calculator_saved_calculations;
