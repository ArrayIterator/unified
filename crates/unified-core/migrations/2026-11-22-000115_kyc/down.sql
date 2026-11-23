-- Down migration for 2026-11-22-000115_kyc: drop all KYC tables.
DROP TABLE IF EXISTS kyc_settings;
DROP TABLE IF EXISTS kyc_audit_entries;
DROP TABLE IF EXISTS kyc_risk_levels;
DROP TABLE IF EXISTS kyc_instructions;
DROP TABLE IF EXISTS kyc_verification_contexts;
DROP TABLE IF EXISTS kyc_subject_types;
DROP TABLE IF EXISTS kyc_document_types;
DROP TABLE IF EXISTS kyc_documents;
DROP TABLE IF EXISTS kyc_cases;
