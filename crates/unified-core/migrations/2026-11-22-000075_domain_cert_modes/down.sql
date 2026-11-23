-- Roll back 2026-11-22-000075_domain_cert_modes, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core and
-- the certificate tables from 2026-11-22-000005_certificates are
-- untouched.

DROP TABLE IF EXISTS cloudflare_ip_ranges;
DROP TABLE IF EXISTS cloudflare_detections;
DROP TABLE IF EXISTS domain_cert_modes;
