-- Spreadsheets module (`src/modules/spreadsheets/`): drop every table
-- created by `2026-11-22-000118_spreadsheets/up.sql`, in reverse
-- dependency order. Like the up migration, this has never
-- been executed against a live PostgreSQL database.

DROP TABLE IF EXISTS spreadsheet_settings;
DROP TABLE IF EXISTS spreadsheet_versions;
DROP TABLE IF EXISTS spreadsheet_shares;
DROP TABLE IF EXISTS spreadsheet_cells;
DROP TABLE IF EXISTS spreadsheet_sheets;
DROP TABLE IF EXISTS spreadsheets;
