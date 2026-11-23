-- Down migration for 2026-11-23-000130_site_builder_package_seed:
-- remove the seeded example package rows only (never the tables).
DELETE FROM saas_package_modules
WHERE package_id = '20000000-0000-4000-8000-000000000002';
DELETE FROM saas_packages
WHERE id = '20000000-0000-4000-8000-000000000002';
