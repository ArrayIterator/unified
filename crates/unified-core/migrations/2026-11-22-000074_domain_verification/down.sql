-- Down migration for 2026-11-22-000074_domain_verification: drop the
-- Domain Verification tables in reverse dependency order.
DROP TABLE IF EXISTS domain_verification_attempts;
DROP TABLE IF EXISTS domain_verification_tokens;
DROP TABLE IF EXISTS domain_verification_settings;
