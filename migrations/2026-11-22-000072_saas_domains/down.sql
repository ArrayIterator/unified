-- Roll back 2026-11-22-000072_saas_domains: drop the three SaaS
-- Domain Management tables (indexes go with their tables).
-- Matches the CREATE count in up.sql: 3 tables.

DROP TABLE IF EXISTS saas_subdomain_allocations;
DROP TABLE IF EXISTS saas_domain_aliases;
DROP TABLE IF EXISTS saas_domain_reservations;
