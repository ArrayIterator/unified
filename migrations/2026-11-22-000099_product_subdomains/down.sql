-- Roll back 2026-11-22-000099_product_subdomains: drop the two
-- per-product subdomain tables (indexes go with their tables).
-- Matches the CREATE count in up.sql: 2 tables.

DROP TABLE IF EXISTS saas_product_subdomain_events;
DROP TABLE IF EXISTS saas_product_subdomains;
