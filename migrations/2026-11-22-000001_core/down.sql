-- Roll back the core tables, in reverse dependency order.
-- The seed roles disappear with the roles table.

DROP TABLE IF EXISTS license_activations;
DROP TABLE IF EXISTS licenses;
DROP TABLE IF EXISTS geo_overrides;
DROP TABLE IF EXISTS domains;
DROP TABLE IF EXISTS company_sites;
DROP TABLE IF EXISTS user_roles;
DROP TABLE IF EXISTS users;
DROP TABLE IF EXISTS roles;
DROP TABLE IF EXISTS companies;
