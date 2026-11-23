-- Down migration for 2026-11-22-000097_library: drop the
-- Library tables in reverse dependency order.
DROP TABLE IF EXISTS library_digital_access;
DROP TABLE IF EXISTS library_inventory_discrepancies;
DROP TABLE IF EXISTS library_inventory_audits;
DROP TABLE IF EXISTS library_loan_policies;
DROP TABLE IF EXISTS library_fines;
DROP TABLE IF EXISTS library_holds;
DROP TABLE IF EXISTS library_loans;
DROP TABLE IF EXISTS library_members;
DROP TABLE IF EXISTS library_copies;
DROP TABLE IF EXISTS library_categories;
DROP TABLE IF EXISTS library_items;
