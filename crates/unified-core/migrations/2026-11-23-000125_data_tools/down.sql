-- Reverse of 000125: drop the Data Tools tables and the minimal
-- `document_shares` table created with them.
--
-- The in-memory DashMap stores are authoritative at runtime; dropping
-- these tables only removes the persistence targets. Dropping
-- `document_shares` removes share rows only - documents and audit
-- events are untouched (the cleaner never deletes those either).

DROP TABLE IF EXISTS document_shares;
DROP TABLE IF EXISTS cleanup_runs;
DROP TABLE IF EXISTS cleanup_rules;
