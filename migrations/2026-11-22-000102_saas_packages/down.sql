-- Down migration for 2026-11-22-000102_saas_packages: drop the
-- SaaS Packages tables in reverse dependency order.
DROP TABLE IF EXISTS saas_package_provision_steps;
DROP TABLE IF EXISTS saas_package_purchases;
DROP TABLE IF EXISTS saas_package_modules;
DROP TABLE IF EXISTS saas_packages;
