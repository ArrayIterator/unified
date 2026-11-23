-- Down migration for 2026-11-22-000088_expenses: drop the
-- Expenses tables in reverse dependency order.
DROP TABLE IF EXISTS expense_currency_rates;
DROP TABLE IF EXISTS expense_adjustments;
DROP TABLE IF EXISTS expense_reimbursements;
DROP TABLE IF EXISTS expense_receipts;
DROP TABLE IF EXISTS expenses;
DROP TABLE IF EXISTS expense_reports;
DROP TABLE IF EXISTS expense_policies;
DROP TABLE IF EXISTS expense_categories;
